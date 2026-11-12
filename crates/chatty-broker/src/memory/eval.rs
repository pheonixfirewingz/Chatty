//! Offline evaluation of character memory extraction (ADR 0007).
//!
//! Each fixture holds a transcript, the participants, saved memories, a
//! scripted model output, and the expected memory state after one automatic
//! extraction. Tests replay the scripted output through a mock server; the
//! `--memory-eval` broker option runs the same fixtures against any inference
//! server, such as `chatty-mock-llama --memory-fixtures` or a live model.

use super::*;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct Fixture {
    #[serde(skip)]
    pub name: String,
    pub description: String,
    pub participants: Vec<String>,
    /// Characters that exist but are not in the chat.
    #[serde(default)]
    pub others: Vec<String>,
    #[serde(default)]
    pub saved: Vec<SavedMemory>,
    pub transcript: Vec<Line>,
    /// Model output replayed by mock servers; live runs ignore it.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by tests and chatty-mock-llama")
    )]
    pub scripted: String,
    pub expected: Expected,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SavedMemory {
    pub id: String,
    pub holder: String,
    pub content: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct Line {
    pub author: String,
    pub content: String,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct Expected {
    /// Memories the run should add, each matched by holder and key phrases.
    #[serde(default)]
    pub active: Vec<ExpectedMemory>,
    /// Required status of saved memories after the run.
    #[serde(default)]
    pub statuses: BTreeMap<String, String>,
    /// Phrases no active memory of a holder may contain.
    #[serde(default)]
    pub forbidden: Vec<Forbidden>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ExpectedMemory {
    pub holder: String,
    pub contains: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct Forbidden {
    pub holder: String,
    pub contains: String,
}

/// Outcome of one fixture.
#[derive(Debug, Default)]
pub(crate) struct FixtureReport {
    pub name: String,
    /// Active memories the run added.
    pub produced: usize,
    /// Added memories that match an expected memory.
    pub matched: usize,
    pub expected: usize,
    /// Added memories that repeat another active memory of the same holder.
    pub duplicates: usize,
    /// Failed status, forbidden-phrase, kind, or scope checks, and run errors.
    pub problems: Vec<String>,
}

impl FixtureReport {
    /// Every expectation met with no extra or duplicate memory.
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
            && self.matched == self.expected
            && self.matched == self.produced
            && self.duplicates == 0
    }
}

/// Loads every `*.json` fixture in `directory`, sorted by file name.
pub(crate) fn load_fixtures(directory: &Path) -> Result<Vec<Fixture>> {
    let mut paths = std::fs::read_dir(directory)
        .with_context(|| format!("cannot read {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let mut fixture: Fixture = serde_json::from_slice(&std::fs::read(&path)?)
                .with_context(|| format!("invalid fixture {}", path.display()))?;
            fixture.name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(fixture)
        })
        .collect()
}

fn character_id(name: &str) -> String {
    name.to_lowercase()
}

/// Seeds `app`'s database with one fixture: owner, characters, chat,
/// transcript, and saved memories.
async fn seed(app: &App, fixture: &Fixture) -> Result<()> {
    let db = &app.db;
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES('owner','owner','unused')")
        .execute(db)
        .await?;
    for name in fixture.participants.iter().chain(&fixture.others) {
        sqlx::query("INSERT INTO characters(id,owner_id,name,personality,scenario,system_prompt,example_dialogue,appearance,tags,revision) VALUES(?,'owner',?,'','','','','',X'',1)")
            .bind(character_id(name))
            .bind(name)
            .execute(db)
            .await?;
    }
    let kind = if fixture.participants.len() > 1 {
        ConversationKind::GroupAutomatic
    } else {
        ConversationKind::Direct
    };
    sqlx::query("INSERT INTO conversations(id,owner_id,title,kind,revision) VALUES('chat','owner','Evaluation',?,1)")
        .bind(kind as i32)
        .execute(db)
        .await?;
    for (position, name) in fixture.participants.iter().enumerate() {
        sqlx::query(
            "INSERT INTO participants(conversation_id,character_id,position) VALUES('chat',?,?)",
        )
        .bind(character_id(name))
        .bind(position as i64)
        .execute(db)
        .await?;
    }
    for (index, line) in fixture.transcript.iter().enumerate() {
        let (author_type, author_id) = if line.author == "user" {
            ("user", None)
        } else {
            ("character", Some(character_id(&line.author)))
        };
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,revision) VALUES(?,'chat',?,?,?,?)")
            .bind(format!("message-{}", index + 1))
            .bind(author_type)
            .bind(author_id)
            .bind(&line.content)
            .bind(index as i64 + 1)
            .execute(db)
            .await?;
    }
    for memory in &fixture.saved {
        let source = match memory.source.as_deref() {
            Some("manual") => MemorySource::Manual,
            _ => MemorySource::Automatic,
        };
        sqlx::query("INSERT INTO memories(id,owner_id,conversation_id,character_id,content,revision,source,pinned) VALUES(?,'owner',NULL,?,?,1,?,?)")
            .bind(&memory.id)
            .bind(character_id(&memory.holder))
            .bind(&memory.content)
            .bind(memory_source_value(source))
            .bind(memory.pinned)
            .execute(db)
            .await?;
    }
    Ok(())
}

fn status_label(status: MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Active => "active",
        MemoryStatus::Superseded => "superseded",
        MemoryStatus::Retired => "retired",
    }
}

/// Runs one fixture in `app`, whose database must be empty, and scores it.
pub(crate) async fn evaluate_fixture(app: &App, fixture: &Fixture) -> FixtureReport {
    let mut report = FixtureReport {
        name: fixture.name.clone(),
        expected: fixture.expected.active.len(),
        ..Default::default()
    };
    if let Err(error) = seed(app, fixture).await {
        report.problems.push(format!("seed failed: {error}"));
        return report;
    }
    if let Err(error) = maybe_extract_automatic_memory(app, "owner", "chat").await {
        report.problems.push(format!("extraction failed: {error}"));
    }
    let memories = match list_memories(&app.db, "owner", None, None, true).await {
        Ok(memories) => memories,
        Err(error) => {
            report.problems.push(format!("listing failed: {error}"));
            return report;
        }
    };
    let saved = fixture
        .saved
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let active = memories
        .iter()
        .filter(|memory| memory.status == MemoryStatus::Active)
        .collect::<Vec<_>>();
    let produced = active
        .iter()
        .filter(|memory| !saved.contains(&memory.id.as_str()))
        .copied()
        .collect::<Vec<_>>();
    report.produced = produced.len();

    let mut unmatched = produced.clone();
    for expected in &fixture.expected.active {
        let holder = character_id(&expected.holder);
        let position = unmatched.iter().position(|memory| {
            let content = memory.content.to_lowercase();
            memory.character_id.as_deref() == Some(holder.as_str())
                && expected
                    .contains
                    .iter()
                    .all(|phrase| content.contains(&phrase.to_lowercase()))
        });
        let Some(position) = position else {
            report.problems.push(format!(
                "missing {} memory containing {:?}",
                expected.holder, expected.contains
            ));
            continue;
        };
        let memory = unmatched.remove(position);
        report.matched += 1;
        if let Some(kind) = &expected.kind
            && !memory.kind.label().eq_ignore_ascii_case(kind)
        {
            report.problems.push(format!(
                "{:?} has kind {}, expected {kind}",
                memory.content,
                memory.kind.label()
            ));
        }
        if let Some(scope) = &expected.scope {
            let actual = if memory.conversation_id.is_some() {
                "chat"
            } else {
                "global"
            };
            if actual != scope {
                report.problems.push(format!(
                    "{:?} has scope {actual}, expected {scope}",
                    memory.content
                ));
            }
        }
    }
    for memory in &unmatched {
        report.problems.push(format!(
            "unexpected memory for {}: {:?}",
            memory.character_id.as_deref().unwrap_or("any"),
            memory.content
        ));
    }
    for memory in &produced {
        let duplicate = active.iter().any(|other| {
            other.id != memory.id
                && other.character_id == memory.character_id
                && (same_memory_text(&other.content, &memory.content)
                    || near_duplicate(&other.content, &memory.content))
        });
        report.duplicates += usize::from(duplicate);
    }
    for (id, expected) in &fixture.expected.statuses {
        let actual = memories
            .iter()
            .find(|memory| &memory.id == id)
            .map_or("deleted", |memory| status_label(memory.status));
        if actual != expected {
            report
                .problems
                .push(format!("{id} is {actual}, expected {expected}"));
        }
    }
    for forbidden in &fixture.expected.forbidden {
        let holder = character_id(&forbidden.holder);
        let phrase = forbidden.contains.to_lowercase();
        if active.iter().any(|memory| {
            memory.character_id.as_deref() == Some(holder.as_str())
                && memory.content.to_lowercase().contains(&phrase)
        }) {
            report.problems.push(format!(
                "{} holds forbidden {:?}",
                forbidden.holder, forbidden.contains
            ));
        }
    }
    report
}

/// An app with an empty in-memory database that uses `url` for inference.
pub(crate) async fn evaluation_app(adapters: AdapterHost, url: &str) -> Result<App> {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::migrate!().run(&db).await?;
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,?)")
        .bind(url.trim_end_matches('/'))
        .execute(&db)
        .await?;
    Ok(App::new(db, adapters, PathBuf::new(), [0; 32]))
}

/// Totals across fixtures: precision, recall, and duplicate rate.
pub(crate) fn summary(reports: &[FixtureReport]) -> String {
    let produced = reports.iter().map(|report| report.produced).sum::<usize>();
    let matched = reports.iter().map(|report| report.matched).sum::<usize>();
    let expected = reports.iter().map(|report| report.expected).sum::<usize>();
    let duplicates = reports
        .iter()
        .map(|report| report.duplicates)
        .sum::<usize>();
    let passed = reports.iter().filter(|report| report.passed()).count();
    // A ratio over nothing is undefined, not perfect.
    let ratio = |part: usize, whole: usize| {
        if whole == 0 {
            "n/a".to_owned()
        } else {
            format!("{:.2}", part as f64 / whole as f64)
        }
    };
    format!(
        "{passed}/{} fixtures passed · precision {} ({matched}/{produced}) · recall {} ({matched}/{expected}) · duplicate rate {} ({duplicates}/{produced})",
        reports.len(),
        ratio(matched, produced),
        ratio(matched, expected),
        ratio(duplicates, produced),
    )
}

/// `chatty-broker --memory-eval <dir>`: runs every fixture against the
/// inference server at `url` and prints a report. With `model`, that model
/// serves extraction. Returns whether every fixture passed.
pub(crate) async fn run_memory_evaluation(
    directory: &Path,
    url: &str,
    model: Option<&str>,
) -> Result<bool> {
    let fixtures = load_fixtures(directory)?;
    let mut reports = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let app = evaluation_app(AdapterHost::from_environment()?, url).await?;
        if let Some(model) = model {
            sqlx::query("UPDATE broker_settings SET model=?,memory_model=?")
                .bind(model)
                .bind(model)
                .execute(&app.db)
                .await?;
        }
        let report = evaluate_fixture(&app, fixture).await;
        println!(
            "{} {:<28} produced {:>2} matched {}/{} duplicates {} · {}",
            if report.passed() { "PASS" } else { "FAIL" },
            report.name,
            report.produced,
            report.matched,
            report.expected,
            report.duplicates,
            fixture.description
        );
        for problem in &report.problems {
            println!("     - {problem}");
        }
        reports.push(report);
    }
    println!("{}", summary(&reports));
    Ok(reports.iter().all(FixtureReport::passed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    fn fixture_directory() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/memory_fixtures")
    }

    /// Serves model discovery and answers every completion with `content`.
    async fn scripted_server(content: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = crate::test_net::mock_listener().await;
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let mut socket = tokio::io::BufReader::new(socket);
                let mut first = String::new();
                socket.read_line(&mut first).await.unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    socket.read_line(&mut header).await.unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let body = if first.starts_with("GET /v1/models ") {
                    r#"{"data":[{"id":"test-model"}]}"#.to_owned()
                } else {
                    json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}]})
                        .to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn scripted_fixtures_meet_every_expectation() {
        let fixtures = load_fixtures(&fixture_directory()).unwrap();
        assert!(fixtures.len() >= 8);
        let mut reports = Vec::new();
        for fixture in &fixtures {
            let (url, task) = scripted_server(fixture.scripted.clone()).await;
            let app = evaluation_app(test_adapter_host(), &url).await.unwrap();
            let report = evaluate_fixture(&app, fixture).await;
            task.abort();
            reports.push(report);
        }
        let summary = summary(&reports);
        eprintln!("{summary}");
        for report in &reports {
            assert!(report.passed(), "{}: {report:?}", report.name);
        }
        assert!(summary.starts_with(&format!("{0}/{0} fixtures passed", reports.len())));
    }

    #[test]
    fn scoring_counts_misses_extras_and_duplicates() {
        let report = FixtureReport {
            produced: 3,
            matched: 2,
            expected: 2,
            duplicates: 1,
            ..Default::default()
        };
        assert!(!report.passed());
        assert_eq!(
            summary(&[report]),
            "0/1 fixtures passed · precision 0.67 (2/3) · recall 1.00 (2/2) · duplicate rate 0.33 (1/3)"
        );
    }
}

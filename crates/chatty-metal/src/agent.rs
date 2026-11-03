//! Token-protected debug endpoints, compiled only into debug images (feature
//! `debug-agent`). Production images have none of this code.
//!
//! Uploaded jobs run as an unprivileged user with no capabilities, so they cannot open the
//! disk's device nodes or clear its read-only flag. The kernel also refuses all NVMe
//! passthrough, and it fails any write bio.

use rustix::{
    fs::{Gid, Uid},
    process::{Resource, Rlimit, getpid, setrlimit},
    system::{RebootCommand, reboot},
    thread::{
        CapabilitySet, remove_capability_from_bounding_set, set_no_new_privs, set_thread_gid,
        set_thread_uid,
    },
};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_REPLY_BYTES: usize = 1 << 20;
const MAX_JOB_BYTES: u64 = 256 << 20;
const MAX_JOB_SECONDS: u64 = 600;
const READABLE_ROOTS: [&str; 2] = ["/proc/", "/sys/"];
/// The user and group of debug jobs, which own nothing on the system.
const JOB_ID: u32 = 65534;
/// `CAP_LAST_CAP` in the pinned kernel. A newer capability would stay in the job's bounding
/// set, so raise this when moving to a kernel that adds one.
const LAST_CAPABILITY: u32 = 40;

/// One parsed request head.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub query: String,
    pub token: Option<String>,
    pub args: String,
    pub content_length: u64,
}

impl Head {
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.split("\r\n");
        let mut start = lines.next()?.split(' ');
        let method = start.next()?.to_owned();
        let target = start.next()?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut head = Self {
            method,
            path: path.to_owned(),
            query: query.to_owned(),
            ..Self::default()
        };
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "authorization" => head.token = value.strip_prefix("Bearer ").map(str::to_owned),
                "content-length" => head.content_length = value.parse().ok()?,
                "x-args" => head.args = value.to_owned(),
                _ => {}
            }
        }
        Some(head)
    }

    fn query_value(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
    }
}

/// Compare without leaking the matching prefix through timing.
fn token_matches(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// Answer a `/debug/` request. `body` holds what follows the head.
pub fn handle(
    mut client: impl Write,
    mut body: impl Read,
    head: &Head,
    token: &str,
) -> io::Result<()> {
    if !head
        .token
        .as_deref()
        .is_some_and(|given| token_matches(given, token))
    {
        return reply(&mut client, "401 Unauthorized", b"bad token\n");
    }
    match (head.method.as_str(), head.path.as_str()) {
        ("GET", "/debug/dmesg") => reply(&mut client, "200 OK", &dmesg()),
        ("GET", "/debug/read") => match read_allowed(head.query_value("path").unwrap_or("")) {
            Ok(bytes) => reply(&mut client, "200 OK", &bytes),
            Err(error) => reply(
                &mut client,
                "400 Bad Request",
                format!("{error}\n").as_bytes(),
            ),
        },
        ("POST", "/debug/run") => {
            let seconds = head
                .query_value("timeout")
                .and_then(|text| text.parse().ok())
                .unwrap_or(60);
            match run_job(&mut body, head, seconds) {
                Ok(output) => reply(&mut client, "200 OK", &output),
                Err(error) => reply(
                    &mut client,
                    "400 Bad Request",
                    format!("{error}\n").as_bytes(),
                ),
            }
        }
        ("POST", "/debug/reboot") => {
            if !getpid().is_init() {
                return reply(&mut client, "409 Conflict", b"not PID 1\n");
            }
            reply(&mut client, "200 OK", b"rebooting\n")?;
            client.flush()?;
            // Give the reply time to leave. Nothing was written to the drive, so there is
            // nothing to flush or unmount.
            thread::sleep(Duration::from_millis(300));
            reboot(RebootCommand::Restart).map_err(io::Error::from)
        }
        _ => reply(&mut client, "404 Not Found", b"unknown debug request\n"),
    }
}

fn reply(client: &mut impl Write, status: &str, body: &[u8]) -> io::Result<()> {
    write!(
        client,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    client.write_all(body)
}

fn dmesg() -> Vec<u8> {
    let Ok(mut kmsg) = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open("/dev/kmsg")
    else {
        return b"cannot open /dev/kmsg\n".to_vec();
    };
    let mut log = Vec::new();
    let mut record = [0; 8192];
    loop {
        match kmsg.read(&mut record) {
            Ok(0) => break,
            Ok(len) => {
                // Each record is "level,seq,time,flags;text". Keep time and text.
                let line = String::from_utf8_lossy(&record[..len]);
                let (meta, text) = line.split_once(';').unwrap_or(("", &line));
                let time = meta.split(',').nth(2).unwrap_or("");
                log.extend_from_slice(format!("{time} {text}").as_bytes());
                if log.len() > MAX_REPLY_BYTES {
                    log.drain(..log.len() - MAX_REPLY_BYTES);
                }
            }
            // EPIPE means a record was overwritten while reading. The next read continues.
            Err(error) if error.raw_os_error() == Some(32) => continue,
            Err(_) => break,
        }
    }
    log
}

fn validate_read_path(path: &str) -> io::Result<()> {
    let safe = path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_.:-".contains(&byte));
    let rooted = READABLE_ROOTS.iter().any(|root| path.starts_with(root));
    if !safe || !rooted || path.split('/').any(|part| part == "..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be under /proc or /sys",
        ));
    }
    Ok(())
}

fn read_allowed(path: &str) -> io::Result<Vec<u8>> {
    validate_read_path(path)?;
    if Path::new(path).is_dir() {
        let mut names: Vec<String> = fs::read_dir(path)?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        return Ok((names.join("\n") + "\n").into_bytes());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_REPLY_BYTES as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn run_job(body: &mut impl Read, head: &Head, seconds: u64) -> io::Result<Vec<u8>> {
    if head.content_length == 0 || head.content_length > MAX_JOB_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "job size out of range",
        ));
    }
    // PID 1 stores the job in the RAM-backed root. Off the hardware, use the temp directory.
    let job = if getpid().is_init() {
        PathBuf::from("/debug-job")
    } else {
        env::temp_dir().join("chatty-metal-debug-job")
    };
    let job = job.as_path();
    let mut file = fs::File::create(job)?;
    let copied = io::copy(&mut body.take(head.content_length), &mut file)?;
    if copied != head.content_length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "job upload was cut short",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o755))?;
    drop(file);
    let output = execute(
        job,
        &head.args,
        seconds.clamp(1, MAX_JOB_SECONDS),
        getpid().is_init(),
    );
    let _ = fs::remove_file(job);
    output
}

/// Run a program to its end or the deadline, returning merged output and the outcome.
fn execute(program: &Path, args: &str, seconds: u64, drop_privileges: bool) -> io::Result<Vec<u8>> {
    let (mut reader, writer) = io::pipe()?;
    let mut command = Command::new(program);
    command
        .args(args.split_ascii_whitespace())
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    if drop_privileges {
        // SAFETY: the closure only makes system calls, which are async-signal-safe, and
        // allocates nothing. A failure aborts the exec, so a job never runs privileged.
        unsafe {
            command.pre_exec(|| {
                // GPU work pins memory, and the job will not be able to raise this limit.
                setrlimit(
                    Resource::Memlock,
                    Rlimit {
                        current: None,
                        maximum: None,
                    },
                )?;
                for capability in 0..=LAST_CAPABILITY {
                    remove_capability_from_bounding_set(CapabilitySet::from_bits_retain(
                        1 << capability,
                    ))?;
                }
                // Group first, because changing the user gives up the right to change it.
                set_thread_gid(Gid::from_raw(JOB_ID))?;
                set_thread_uid(Uid::from_raw(JOB_ID))?;
                set_no_new_privs(true)?;
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    drop(command);

    let collector = thread::spawn(move || {
        let mut output = Vec::new();
        let _ = (&mut reader)
            .take(MAX_REPLY_BYTES as u64)
            .read_to_end(&mut output);
        // Keep draining so a chatty job cannot block on a full pipe.
        let _ = io::copy(&mut reader, &mut io::sink());
        output
    });

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break format!("{status}");
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            break format!("killed after {seconds} s");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = collector.join().unwrap_or_default();
    output.extend_from_slice(format!("\n[job: {outcome}]\n").as_bytes());
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const TOKEN: &str = "secret";

    fn call(method: &str, path: &str, token: Option<&str>) -> String {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let head = Head {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            token: token.map(str::to_owned),
            ..Head::default()
        };
        let mut reply = Vec::new();
        handle(&mut reply, Cursor::new(Vec::new()), &head, TOKEN).unwrap();
        String::from_utf8(reply).unwrap()
    }

    #[test]
    fn parses_request_head() {
        let head = Head::parse(
            "POST /debug/run?timeout=5 HTTP/1.1\r\nAuthorization: Bearer abc\r\nContent-Length: 12\r\nX-Args: a b\r\n",
        )
        .unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/debug/run");
        assert_eq!(head.query_value("timeout"), Some("5"));
        assert_eq!(head.token.as_deref(), Some("abc"));
        assert_eq!(head.content_length, 12);
        assert_eq!(head.args, "a b");
    }

    #[test]
    fn parses_path_without_query() {
        assert_eq!(
            Head::parse("GET /debug/dmesg HTTP/1.1\r\n").unwrap().path,
            "/debug/dmesg"
        );
    }

    #[test]
    fn rejects_missing_or_wrong_token() {
        assert!(call("GET", "/debug/dmesg", None).starts_with("HTTP/1.1 401"));
        assert!(call("GET", "/debug/dmesg", Some("nope!!")).starts_with("HTTP/1.1 401"));
        assert!(call("GET", "/debug/dmesg", Some("secreT")).starts_with("HTTP/1.1 401"));
    }

    #[test]
    fn read_is_limited_to_proc_and_sys() {
        for path in [
            "/etc/passwd",
            "/dev/nvme0n1",
            "/model/x",
            "/proc/../etc/passwd",
            "/proc/a b",
            "",
        ] {
            let reply = call("GET", &format!("/debug/read?path={path}"), Some(TOKEN));
            assert!(reply.starts_with("HTTP/1.1 400"), "{path}: {reply}");
        }
        let reply = call("GET", "/debug/read?path=/proc/version", Some(TOKEN));
        assert!(
            reply.starts_with("HTTP/1.1 200") && reply.contains("Linux"),
            "{reply}"
        );
    }

    #[test]
    fn reboot_is_refused_outside_pid_1() {
        assert!(call("POST", "/debug/reboot", Some(TOKEN)).starts_with("HTTP/1.1 409"));
    }

    #[test]
    fn unknown_debug_request_is_404() {
        assert!(call("GET", "/debug/other", Some(TOKEN)).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn reports_exit_status() {
        let output = execute(Path::new("/bin/false"), "", 5, false).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("exit status: 1"), "{text}");
    }

    #[test]
    fn kills_job_past_deadline() {
        let output = execute(Path::new("/bin/sleep"), "30", 1, false).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("killed after 1 s")
        );
    }

    #[test]
    fn rejects_empty_job() {
        let head = Head {
            method: "POST".into(),
            path: "/debug/run".into(),
            token: Some(TOKEN.into()),
            ..Head::default()
        };
        let mut reply = Vec::new();
        handle(&mut reply, Cursor::new(Vec::new()), &head, TOKEN).unwrap();
        assert!(
            String::from_utf8(reply)
                .unwrap()
                .starts_with("HTTP/1.1 400")
        );
    }
}

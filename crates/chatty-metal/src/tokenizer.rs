//! The model's `tokenizer.json`: GPT-2 style byte-level BPE behind three regex splits, plus the
//! compressed token map that the engram n-gram hashes are defined over.

use crate::{Error, Result, invalid};
use fancy_regex::Regex;
use serde::Deserialize;
use std::{collections::HashMap, fs, path::Path};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// Private-use character the reference normalizer maps back to a space after stripping.
const SENTINEL: char = '\u{e000}';

#[derive(Deserialize)]
struct File {
    added_tokens: Vec<Added>,
    pre_tokenizer: PreTokenizer,
    model: Model,
}

#[derive(Deserialize)]
struct Added {
    id: u32,
    content: String,
}

#[derive(Deserialize)]
struct PreTokenizer {
    pretokenizers: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    pattern: Option<Pattern>,
}

#[derive(Deserialize)]
struct Pattern {
    #[serde(rename = "Regex")]
    regex: String,
}

#[derive(Deserialize)]
struct Model {
    vocab: HashMap<String, u32>,
    merges: Vec<String>,
}

pub struct Tokenizer {
    /// Id to token text; empty for ids the file does not name.
    tokens: Vec<String>,
    specials: HashMap<String, u32>,
    byte_ids: [u32; 256],
    /// Pair of ids to (rank, merged id).
    merges: HashMap<(u32, u32), (u32, u32)>,
    splits: Vec<Regex>,
    char_bytes: HashMap<char, u8>,
}

impl Tokenizer {
    pub fn open(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let file: File = serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("tokenizer.json: {e}")))?;
        let size = file
            .model
            .vocab
            .values()
            .chain(file.added_tokens.iter().map(|a| &a.id))
            .max()
            .map_or(0, |max| *max as usize + 1);
        let mut tokens = vec![String::new(); size];
        for (token, &id) in &file.model.vocab {
            tokens[id as usize].clone_from(token);
        }
        let mut specials = HashMap::with_capacity(file.added_tokens.len());
        for added in file.added_tokens {
            tokens[added.id as usize].clone_from(&added.content);
            specials.insert(added.content, added.id);
        }

        let table = byte_chars();
        let mut byte_ids = [0; 256];
        let mut char_bytes = HashMap::with_capacity(256);
        for (byte, &c) in table.iter().enumerate() {
            let name = c.to_string();
            let Some(&id) = file.model.vocab.get(&name) else {
                return invalid(format!("tokenizer has no token for byte {byte}"));
            };
            byte_ids[byte] = id;
            char_bytes.insert(c, byte as u8);
        }

        let mut merges = HashMap::with_capacity(file.model.merges.len());
        for (rank, merge) in file.model.merges.iter().enumerate() {
            let Some((a, b)) = merge.split_once(' ') else {
                return invalid(format!("bad merge {merge:?}"));
            };
            let id =
                |name: &str| {
                    file.model.vocab.get(name).copied().ok_or_else(|| {
                        Error::Invalid(format!("merge names unknown token {name:?}"))
                    })
                };
            merges.insert((id(a)?, id(b)?), (rank as u32, id(&format!("{a}{b}"))?));
        }

        let mut splits = Vec::new();
        for step in file.pre_tokenizer.pretokenizers {
            if let Some(pattern) = step.pattern {
                let regex = Regex::new(&pattern.regex)
                    .map_err(|e| Error::Invalid(format!("tokenizer regex: {e}")))?;
                splits.push(regex);
            }
        }
        Ok(Self {
            tokens,
            specials,
            byte_ids,
            merges,
            splits,
            char_bytes,
        })
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The id of a special token, by its exact text.
    pub fn special(&self, name: &str) -> Result<u32> {
        self.specials
            .get(name)
            .copied()
            .ok_or_else(|| Error::Invalid(format!("tokenizer has no special token {name:?}")))
    }

    /// Encode text with special tokens treated as plain text, so user content cannot forge them.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut pieces = vec![text];
        for regex in &self.splits {
            let mut next = Vec::with_capacity(pieces.len());
            for piece in pieces {
                split_isolated(regex, piece, &mut next);
            }
            pieces = next;
        }
        let mut out = Vec::with_capacity(text.len() / 3);
        for piece in pieces {
            self.bpe(piece.as_bytes(), &mut out);
        }
        out
    }

    fn bpe(&self, bytes: &[u8], out: &mut Vec<u32>) {
        let mut symbols: Vec<u32> = bytes.iter().map(|&b| self.byte_ids[b as usize]).collect();
        while symbols.len() > 1 {
            let best = symbols
                .windows(2)
                .filter_map(|pair| self.merges.get(&(pair[0], pair[1])).map(|m| (m.0, pair)))
                .min_by_key(|(rank, _)| *rank);
            let Some((rank, pair)) = best else {
                break;
            };
            let (a, b) = (pair[0], pair[1]);
            let merged = self.merges[&(a, b)].1;
            debug_assert_eq!(self.merges[&(a, b)].0, rank);
            let mut write = 0;
            let mut read = 0;
            while read < symbols.len() {
                if read + 1 < symbols.len() && symbols[read] == a && symbols[read + 1] == b {
                    symbols[write] = merged;
                    read += 2;
                } else {
                    symbols[write] = symbols[read];
                    read += 1;
                }
                write += 1;
            }
            symbols.truncate(write);
        }
        out.extend_from_slice(&symbols);
    }

    /// The bytes of a token. Text that is not byte-level, such as a special token, stays as UTF-8.
    pub fn token_bytes(&self, id: u32) -> Vec<u8> {
        let Some(token) = self.tokens.get(id as usize) else {
            return Vec::new();
        };
        token
            .chars()
            .map(|c| self.char_bytes.get(&c).copied())
            .collect::<Option<Vec<u8>>>()
            .unwrap_or_else(|| token.as_bytes().to_vec())
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let bytes: Vec<u8> = ids.iter().flat_map(|&id| self.token_bytes(id)).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Map every id to a smaller space where tokens that normalize alike collapse together.
    /// Returns the map and that space's size, which the engram hash multipliers depend on.
    pub fn compressed_map(&self) -> (Vec<u32>, u32) {
        let mut keys: HashMap<String, u32> = HashMap::with_capacity(self.tokens.len());
        let mut map = Vec::with_capacity(self.tokens.len());
        for (id, token) in self.tokens.iter().enumerate() {
            let text = self.decode(&[id as u32]);
            let key = if text.contains('\u{fffd}') {
                token.clone()
            } else {
                let normalized = normalize_key(&text);
                if normalized.is_empty() {
                    text
                } else {
                    normalized
                }
            };
            let next = keys.len() as u32;
            map.push(*keys.entry(key).or_insert(next));
        }
        (map, keys.len() as u32)
    }
}

/// Keep matches and the gaps between them, in order, as separate pieces.
fn split_isolated<'a>(regex: &Regex, text: &'a str, out: &mut Vec<&'a str>) {
    let mut last = 0;
    for found in regex.find_iter(text) {
        // The engine only errors past its backtrack limit; treat the rest as one piece.
        let Ok(found) = found else {
            break;
        };
        if found.start() > last {
            out.push(&text[last..found.start()]);
        }
        if found.end() > found.start() {
            out.push(found.as_str());
        }
        last = found.end();
    }
    if last < text.len() {
        out.push(&text[last..]);
    }
}

/// NFKC, NFD, strip accents, lowercase, collapse whitespace, strip. A token that is exactly one
/// space survives the strip, so it does not merge with unrelated tokens.
fn normalize_key(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let folded: String = nfkc
        .nfd()
        .filter(|&c| !is_combining_mark(c))
        .flat_map(char::to_lowercase)
        .collect();
    let mut collapsed = String::with_capacity(folded.len());
    let mut in_space = false;
    for c in folded.chars() {
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    if collapsed == " " {
        return " ".to_owned();
    }
    collapsed.trim().replace(SENTINEL, " ")
}

/// GPT-2's reversible map from bytes to printable characters.
fn byte_chars() -> [char; 256] {
    let mut table = ['\0'; 256];
    let mut extra = 0;
    for byte in 0..=255u32 {
        let printable = matches!(byte, 33..=126 | 161..=172 | 174..=255);
        let code = if printable {
            byte
        } else {
            extra += 1;
            255 + extra
        };
        table[byte as usize] = char::from_u32(code).unwrap_or('\0');
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "added_tokens": [{"id": 300, "content": "<｜User｜>"}],
        "pre_tokenizer": {"pretokenizers": [
            {"pattern": {"Regex": "\\p{N}{1,3}"}},
            {"pattern": {"Regex": "\\s+(?!\\S)|\\s+"}},
            {}
        ]},
        "model": {"vocab": VOCAB, "merges": ["h e", "he l", "hel l", "hell o"]}
    }"#;

    fn tokenizer() -> Tokenizer {
        let mut vocab = serde_json::Map::new();
        for (byte, c) in byte_chars().iter().enumerate() {
            vocab.insert(c.to_string(), byte.into());
        }
        for (index, name) in ["he", "hel", "hell", "hello"].iter().enumerate() {
            vocab.insert((*name).to_owned(), (256 + index).into());
        }
        let text = FIXTURE.replace("VOCAB", &serde_json::Value::Object(vocab).to_string());
        Tokenizer::parse(&text).unwrap()
    }

    #[test]
    fn merges_in_rank_order_and_round_trips() {
        let t = tokenizer();
        let ids = t.encode("hello world");
        assert_eq!(ids[0], 259);
        assert_eq!(t.decode(&ids), "hello world");
    }

    #[test]
    fn splits_numbers_into_runs_of_three() {
        let t = tokenizer();
        let mut pieces = Vec::new();
        split_isolated(&t.splits[0], "a12345b", &mut pieces);
        assert_eq!(pieces, ["a", "123", "45", "b"]);
    }

    #[test]
    fn special_text_is_not_a_special_token() {
        let t = tokenizer();
        let ids = t.encode("<｜User｜>");
        assert!(!ids.contains(&t.special("<｜User｜>").unwrap()));
    }

    #[test]
    fn specials_decode_as_their_text() {
        let t = tokenizer();
        assert_eq!(t.decode(&[300]), "<｜User｜>");
    }

    #[test]
    fn utf8_split_across_tokens_rejoins() {
        let t = tokenizer();
        let ids = t.encode("é");
        assert_eq!(ids.len(), 2);
        assert_eq!(t.decode(&ids), "é");
    }

    #[test]
    fn normalization_folds_case_accents_and_space() {
        assert_eq!(normalize_key(" The"), "the");
        assert_eq!(normalize_key("É"), "e");
        assert_eq!(normalize_key("  "), " ");
        assert_eq!(normalize_key("a \n b"), "a b");
    }
}

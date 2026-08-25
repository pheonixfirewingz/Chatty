//! Minimal command-line parsing, replacing the `clap` derive.
//!
//! Understands `--flag`, `--flag value`, and `--flag=value`; rejects
//! anything else (including bare positionals) so typos fail loudly.
//! Resolution order mirrors clap: command line > environment > default.

use std::collections::HashMap;

use super::Error;

#[derive(Default)]
pub struct ParsedArgs {
    values: HashMap<String, Option<String>>,
}

impl ParsedArgs {
    /// Parses the process arguments, printing `usage` for `--help`.
    pub fn parse(usage: &str) -> Result<Self, Error> {
        let mut values: HashMap<String, Option<String>> = HashMap::new();
        let mut arguments = std::env::args().skip(1).peekable();
        while let Some(argument) = arguments.next() {
            let name = argument
                .strip_prefix("--")
                .ok_or_else(|| Error::msg(format!("unexpected argument: {argument}")))?;
            if name == "help" {
                println!("{usage}");
                std::process::exit(0);
            }
            let value = match name.split_once('=') {
                Some((_, inline)) => Some(inline.to_owned()),
                None => arguments
                    .next_if(|next| !next.starts_with("--")),
            };
            values.insert(name.split('=').next().unwrap().to_owned(), value);
        }
        Ok(Self { values })
    }

    /// The raw token after `--name`, if one was attached.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(Option::as_deref)
    }

    /// Whether `--name` appeared at all.
    pub fn flag(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// Resolves a string option: CLI value, then env var, then default.
    pub fn string(&self, name: &str, env_key: &str, default: &str) -> String {
        self.value(name)
            .map(str::to_owned)
            .or_else(|| std::env::var(env_key).ok())
            .unwrap_or_else(|| default.to_owned())
    }

    /// Resolves an optional option: CLI value, else env var.
    pub fn optional(&self, name: &str, env_key: &str) -> Option<String> {
        self.value(name)
            .map(str::to_owned)
            .or_else(|| std::env::var(env_key).ok())
    }

    /// Like [`Self::string`] but parsed as a number, falling back to
    /// `default` when absent or malformed.
    pub fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> T {
        self.value(name).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

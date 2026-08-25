//! A tiny std-only stand-in for the `anyhow` crate.
//!
//! Covers the dynamic-error patterns this workspace actually uses: an owned
//! [`Error`] carrying layered context messages over a source chain, a
//! [`Result`] alias, `.context()` adapters for `Result` and `Option`, and the
//! [`bail!`] / [`format_err!`] macros. Deliberately mirrors `anyhow`'s
//! semantics so call sites read the same without the dependency.

use std::error::Error as StdError;
use std::fmt;

/// Owned dynamic error: either a bare message or a message wrapped around
/// another error's chain.
///
/// Like `anyhow::Error`, this intentionally does *not* implement `StdError`;
/// doing so would make the blanket `From<E: StdError>` conversion overlap
/// with the reflexive `From<T> for T` impl.
#[derive(Debug)]
pub struct Error(Box<dyn StdError + Send + Sync + 'static>);

impl Error {
    /// Creates an error from a message alone (see also [`format_err!`]).
    pub fn msg(message: impl Into<String>) -> Self {
        Self(Box::new(BareMessage(message.into())))
    }

    /// Adds one context layer above whatever this error already carries.
    fn contextual(self, message: String) -> Self {
        Self(Box::new(ContextLayer {
            message,
            source: self.0,
        }))
    }
}

impl fmt::Display for Error {
    /// Renders only the outermost message, like `anyhow`; walk `.source()`
    /// for the full chain.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Any standard error converts into [`Error`] with `?`, becoming the base of
/// the chain so nothing is lost.
impl<E: StdError + Send + Sync + 'static> From<E> for Error {
    fn from(error: E) -> Self {
        Self(Box::new(error))
    }
}

/// Leaf of an error chain: just a message with nothing beneath it.
#[derive(Debug)]
struct BareMessage(String);

impl fmt::Display for BareMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl StdError for BareMessage {}

/// One `.context()` layer: a message plus the error it decorates.
#[derive(Debug)]
struct ContextLayer {
    message: String,
    source: Box<dyn StdError + Send + Sync + 'static>,
}

impl fmt::Display for ContextLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for ContextLayer {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Convenience alias mirroring `anyhow::Result`.
pub type Result<T> = std::result::Result<T, Error>;

/// Adds context messages to `Result` and `Option` values, like
/// `anyhow::Context`.
pub trait Context<T> {
    /// Attaches `message` to the error, or turns `None` into an error.
    fn context(self, message: impl Into<String>) -> Result<T>;

    /// Lazily builds the message only when the value is an error/`None`.
    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.map_err(|error| error.into().contextual(message.into()))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|error| error.into().contextual(f()))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| Error::msg(message))
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.ok_or_else(|| Error::msg(f()))
    }
}

/// Returns early with [`Error::msg`], like `anyhow::bail!`.
///
/// The argument is always treated as a `format!` template so inline captured
/// identifiers (`bail!("bad value: {value}")`) work.
#[macro_export]
macro_rules! bail {
    ($format:expr $(,)?) => {
        return ::std::result::Result::Err($crate::util::Error::msg(format!($format)))
    };
    ($format:expr, $($argument:tt)*) => {
        return ::std::result::Result::Err($crate::util::Error::msg(format!($format, $($argument)*)))
    };
}

/// Builds a formatted [`Error`] value, standing in for `anyhow::anyhow!`.
#[macro_export]
macro_rules! format_err {
    ($format:expr $(,)?) => {
        $crate::util::Error::msg(format!($format))
    };
    ($format:expr, $($argument:tt)*) => {
        $crate::util::Error::msg(format!($format, $($argument)*))
    };
}

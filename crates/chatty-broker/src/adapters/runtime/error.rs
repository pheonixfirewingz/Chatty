use std::fmt;

#[derive(Debug)]
pub struct Error(String);
impl Error {
    pub(crate) fn new(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
macro_rules! conversion {
    ($($kind:ty),*) => { $(impl From<$kind> for Error {
        fn from(error: $kind) -> Self { Self::new(error) }
    })* };
}
conversion!(
    std::io::Error,
    serde_json::Error,
    reqwest::Error,
    wasmi::Error,
    std::str::Utf8Error
);
pub type Result<T> = std::result::Result<T, Error>;

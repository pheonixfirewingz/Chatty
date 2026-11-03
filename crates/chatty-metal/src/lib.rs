//! Single-model inference appliance for DeepSeek-V4.1-Flash on one RX 7900 XTX.
//! See docs/architecture/adr/0007-bare-metal-inference-appliance.md.
#![deny(unsafe_code)]

#[cfg(feature = "debug-agent")]
#[allow(unsafe_code)]
pub mod agent;
#[allow(unsafe_code)]
pub mod boot;
pub mod catalog;
#[allow(unsafe_code)]
pub mod cpu_fp4;
pub mod engram;
#[allow(unsafe_code)]
pub mod gpu_model;
#[allow(unsafe_code)]
pub mod kfd;
#[allow(unsafe_code)]
pub mod monitor;
pub mod plan;
pub mod reference;
#[allow(unsafe_code)]
pub mod residency;
pub mod routing;
pub mod server;
#[allow(unsafe_code)]
pub mod store;
pub mod tokenizer;

use std::{fmt, io, path::PathBuf};

#[derive(Debug)]
pub enum Error {
    Io(PathBuf, io::Error),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

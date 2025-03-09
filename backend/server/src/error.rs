use std::num::ParseIntError;
use std::env::VarError;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    Ocotocrab(octocrab::Error),
    ParseInt(ParseIntError),
    EnvVar(VarError),
    StdIO(std::io::Error),
    SerdeJson(serde_json::Error),
}

impl From<VarError> for self::Error {
    fn from(err: VarError) -> Self {
        Self::EnvVar(err)
    }
}

impl From<ParseIntError> for self::Error {
    fn from(err: ParseIntError) -> Self {
        Self::ParseInt(err)
    }
}

impl From<std::io::Error> for self::Error {
    fn from(err: std::io::Error) -> Self {
        Self::StdIO(err)
    }
}

impl From<serde_json::Error> for self::Error {
    fn from(err: serde_json::Error) -> Self {
        Self::SerdeJson(err)
    }
}

impl From<octocrab::Error> for self::Error {
    fn from(err: octocrab::Error) -> Self {
        Error::Ocotocrab(err)
    }
}

pub type Result<T, E = self::Error> = std::result::Result<T, E>;

pub trait LogResult {
    fn log_with_context(self, context: &str);
}

impl<T> LogResult for Result<T> {
    fn log_with_context(self, context: &str) {
        match self {
            Ok(_) => {
                log::info!("Attempting {} was successful", context);
            }
            Err(e) => {
                log::error!("When attempting to {}, an error was encountered: {:?}", context, e);
            }
        }
    }
}

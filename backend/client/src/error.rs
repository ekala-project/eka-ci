use std::num::ParseIntError;
use std::env::VarError;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    ParseInt(ParseIntError),
    EnvVar(VarError),
    SerdeJson(serde_json::Error),
    StdIO(std::io::Error),
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

impl From<serde_json::Error> for self::Error {
    fn from(err: serde_json::Error) -> Self {
        Self::SerdeJson(err)
    }
}

impl From<std::io::Error> for self::Error {
    fn from(err: std::io::Error) -> Self {
        Self::StdIO(err)
    }
}

pub type Result<T, E = self::Error> = std::result::Result<T, E>;

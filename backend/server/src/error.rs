
use std::num::ParseIntError;
use std::env::VarError;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    Ocotocrab(octocrab::Error),
    ParseInt(ParseIntError),
    EnvVar(VarError),
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

impl From<octocrab::Error> for self::Error {
    fn from(err: octocrab::Error) -> Self {
        Error::Ocotocrab(err)
    }
}

pub type Result<T, E = self::Error> = std::result::Result<T, E>;

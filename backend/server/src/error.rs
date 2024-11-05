
use octocrab;
use std::num::ParseIntError;
use std::env::VarError;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    Ocotocrab(octocrab::Error),
    ParseIntError(ParseIntError),
    EnvVarError(VarError),
}

impl From<VarError> for self::Error {
    fn from(err: VarError) -> Self {
        return Self::EnvVarError(err)
    }
}

impl From<ParseIntError> for self::Error {
    fn from(err: ParseIntError) -> Self {
        return Self::ParseIntError(err)
    }
}

impl From<octocrab::Error> for self::Error {
    fn from(err: octocrab::Error) -> Self {
        return Error::Ocotocrab(err)
    }
}

pub type Result<T, E = self::Error> = std::result::Result<T, E>;

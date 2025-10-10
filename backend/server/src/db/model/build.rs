//! Data structures for a derivation build.
//!
//! A derivation build is the process of realizing a derivation in the Nix store to check if it
//! builds successfully. Each build attempt is uniquely identified by a [`DrvBuildId`].
//!
//! For each attempt to build a derivation a new [`DrvBuildMetadata`] entry is stored in the
//! database.
//!
//! During the build process several [`DrvBuildEvent`] entries are inserted into the database. The
//! latest of these entries is the current build status.
use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sqlx::encode::IsNull;
use sqlx::sqlite::SqliteArgumentValue;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};

use super::ForInsert;
use crate::db::model::drv_id::DrvId;
use crate::db::model::git::{GitCommit, GitRepo};

/// Unique identifier for a derivation build attempt.
///
/// Combines the derivation identifier with a counter that keeps track of the number of build
/// attempts for that derivation.
#[derive(Clone, Debug, FromRow)]
pub struct DrvBuildId {
    /// The derivation that is attempted to be build.
    pub derivation: DrvId,

    /// The build attempt counter.
    ///
    /// This value is increased for each new attempt at building the derivation.
    ///
    /// Note that once the build outcome of a derivation has been determined, there is no point in
    /// trying to build the same derivation again. If it failed once, it will always fail.
    ///
    /// This counter is intended for cases in which the derivation build was interrupted due to
    /// external factors (see [`DrvBuildState::Interrupted`]). In these situations it may make
    /// sense to reattempt the build (depending on the interruption kind).
    pub build_attempt: NonZeroU32,
}

/// Metadata about a derivation build.
///
/// This metadata is useful to reproduce a build on a different machine. Note that in general,
/// only builds that ended with a state of [`DrvBuildState::Completed`] can be reproduced.
#[derive(Clone, Debug, FromRow)]
pub struct DrvBuildMetadata {
    /// The derivation build this metadata is associated with.
    #[sqlx(flatten)]
    pub build: DrvBuildId,

    /// The Git repository this derivation build originates from.
    pub git_repo: GitRepo,

    /// The Git commit this derivation build originates from.
    ///
    /// Note that this may not be the only commit that can produce this derivation. Because a
    /// derivation only needs to fully build once, later commits may still include this
    /// derivation but do not trigger a new build.
    pub git_commit: GitCommit,

    /// The Nix command that was used to build this derivation.
    pub build_command: DrvBuildCommand,
}

impl DrvBuildMetadata {
    pub fn for_insert(
        derivation: DrvId,
        git_repo: GitRepo,
        git_commit: GitCommit,
        build_command: DrvBuildCommand,
    ) -> ForInsert<Self> {
        ForInsert(Self {
            build: DrvBuildId {
                derivation,
                build_attempt: NonZeroU32::MAX,
            },
            git_repo,
            git_commit,
            build_command,
        })
    }
}

/// Command used to build the derivation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")] // use internally tagged serialization
pub enum DrvBuildCommand {
    /// Build a single attribute.
    SingleAttr {
        /// Path to the Nix executable.
        ///
        /// Since this will be a Nix store path, it conveniently also includes the executable's
        /// version and unique identifier.
        exec: PathBuf,
        /// Nix arguments.
        args: Vec<String>,
        /// Environment variables for the subprocess.
        env: HashMap<String, String>,
        /// The `.nix` file that contains the attribute.
        file: PathBuf,
        /// The attribute to build.
        attr: String,
    },
}

#[cfg(test)]
impl DrvBuildCommand {
    /// Returns a dummy build command. Useful for database inserts in tests.
    pub fn dummy() -> Self {
        Self::SingleAttr {
            exec: "/bin/nix".into(),
            args: Vec::new(),
            env: HashMap::new(),
            file: "/path/to/file.nix".into(),
            attr: "hello".to_owned(),
        }
    }
}

impl<'q> Encode<'q, Sqlite> for DrvBuildCommand {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let encoded = serde_json::to_string(self)?;
        buf.push(SqliteArgumentValue::Text(Cow::Owned(encoded)));

        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, Sqlite> for DrvBuildCommand {
    fn decode(
        value: <Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <&str as Decode<Sqlite>>::decode(value)?;
        let command = serde_json::from_str(value)?;

        Ok(command)
    }
}

impl Type<Sqlite> for DrvBuildCommand {
    fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
        <str as Type<Sqlite>>::type_info()
    }
}

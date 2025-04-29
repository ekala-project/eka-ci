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
use std::{borrow::Cow, collections::HashMap, num::NonZeroU32, path::PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::{encode::IsNull, sqlite::SqliteArgumentValue, Decode, Encode, FromRow, Sqlite, Type};

use crate::db::model::git::{GitCommit, GitRepo};

use super::ForInsert;

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
    /// external factors (see [`DrvBuildState::Interrupted`]). In these situations it may make sense
    /// to reattempt the build (depending on the interruption kind).
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

/// Emitted whenever a derivation build's state changes.
#[derive(Clone, Debug, FromRow)]
pub struct DrvBuildEvent {
    /// The derivation build this event is associated with.
    #[sqlx(flatten)]
    pub build: DrvBuildId,

    /// The build state this event propagates.
    pub state: DrvBuildState,

    /// The timestamp when this event happened.
    ///
    /// This timestamp only has second accuracy, which makes it unsuitable for sorting of build
    /// events. If for example the build queue is empty, it is not unlikely that a build is
    /// scheduled ([`DrvBuildState::Pending`]) and started ([`DrvBuildState::Building`]) within the
    /// same second.
    ///
    /// Instead, use the table's ROWID to sort the events during select.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl DrvBuildEvent {
    pub fn for_insert(build: DrvBuildId, state: DrvBuildState) -> ForInsert<Self> {
        ForInsert(Self {
            build,
            state,
            timestamp: chrono::DateTime::<chrono::Utc>::MAX_UTC,
        })
    }
}

/// Describes the possible states a derivation build can be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildState {
    /// Derivation is waiting to be scheduled for building.
    ///
    /// The evaluator has determined that this derivation needs be built and has sent it to the
    /// scheduler. The derivation stays in this state until the scheduler decides that it is ready
    /// to be built, which mostly means until all its dependencies have been built.
    Queued,
    /// Derivation is waiting to be built.
    ///
    /// The scheduler has determined that this derivation is ready to be built. The derivation
    /// stays in this state until a builder picks it up to perform the actual build step.
    Buildable,
    /// Derivation is building.
    ///
    /// A builder has picked this derivation up and is now realizing the derivation. The derivation
    /// build stays in this state until the build completes or is interrupted.
    Building,
    /// Derivation has been built, either successfully or not.
    ///
    /// This is a terminal state, a derivation build will never leave this state. Depending on the
    /// outcome of the built, the state of other derivation builds may be changed. If the build
    /// completed successfully, all direct dependants will be marked as buildable. If the build
    /// failed, all transitive dependants will be marked as transitive failure.
    Completed(DrvBuildResult),
    /// Build was interrupted before it could complete.
    ///
    /// For some interruption kinds, the build will be retried automatically. In those cases, the
    /// build will be immediately marked as buildable again. Dependants are not affected.
    ///
    /// For most interruption kinds however, an automatic retry makes no sense. A new attempt at
    /// building the derivation may be queued manually or when the job configuration changed. All
    /// transitive dependants of this derivation will be marked as blocked, until the next build
    /// attempt. This derivation build will never leave this state in that case.
    Interrupted(DrvBuildInterruptionKind),
    /// At least one transitive dependency of this build has failed.
    ///
    /// This is a terminal state, a derivation build will never leave this state.
    TransitiveFailure,
    /// At least one transitive dependency of this build has been interrupted.
    ///
    /// A failing build of another transitive dependency has a higher precedence than this. The
    /// transitive failure state therefore takes priority over this state and overwrite it.
    ///
    /// Otherwise, the derivation build stays in this state until a later build attempt of the
    /// dependency completes. Every time a build attempt completes, the scheduler checks if a
    /// previous build attempt has been interrupted, and if so, unblocks all transitive dependants
    /// again. Once a derivation build is unblocked, it will be queued again.
    Blocked,
}

/// The result of building a derivation.
///
/// In essence, this enum captures whether the status code returned by the build command was `0`
/// or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildResult {
    /// The derivation built successfully.
    Success,
    /// The derivation failed to build.
    Failure,
}

impl DrvBuildResult {
    /// Handy helper that allows processing the build result in a more functional style using
    /// [map][Result::map], [map_err][Result::map_err], [map_or_else][Result::map_or_else] and
    /// the like.
    pub fn as_result(&self) -> Result<(), ()> {
        match self {
            DrvBuildResult::Success => Ok(()),
            DrvBuildResult::Failure => Err(()),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure)
    }
}

/// Possible causes for why the derivation build was interrupted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildInterruptionKind {
    /// Build process ran out of memory and was killed by the system.
    OutOfMemory,
    /// Build process timed out and was killed by the build scheduler.
    Timeout,
    /// Scheduler process performed a graceful shutdown and cancelled the derivation build in the
    /// process.
    Cancelled,
    /// Build process died for unknown reasons, most likely a fault in the build command.
    ProcessDeath,
    /// Scheduler process died. The scheduler can infer that this happend by checking for
    /// derivation builds which do not have the status [`DrvBuildState::Completed`] whilst
    /// starting.
    SchedulerDeath,
}

/// A derivation identifier of the form `hash-name.drv`.
///
/// Many derivations that describe a package (binaries, libraries, ...) additionally include a
/// version identifier in the name component. For these derivations, the identifier often looks
/// like `hash-name-version.drv`. This is however only a convention. Many intermediate build
/// artifacts for example do not have a version.
///
/// Each derivation identifier corresponds to a file with the same name located in a nix store. The
/// filesystem path of the store depends on the evaluator that produced the derivation and is part
/// of the identifier's hash component[^nix-by-hand]. It is not possible to determine the store
/// path given only a derivation identifier.
///
/// # Examples
///
/// Derivation for the hello package, version 2.12.1:
/// `jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv`
///
/// Derivation for the source of an unknown other derivation:
/// `0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv`
///
/// [^nix-by-hand]: <https://bernsteinbear.com/blog/nix-by-hand/>
#[derive(Clone, Debug, PartialEq, Eq, Type)]
#[sqlx(transparent)]
pub struct DrvId(String);

/// Constructors and methods useful for testing.
#[cfg(test)]
impl DrvId {
    /// Returns a known good derivation identifier. Useful for database inserts in tests.
    pub fn dummy() -> Self {
        DrvId("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv".to_owned())
    }
}

mod state {
    use sqlx::{Decode, Encode, Sqlite, Type};

    use super::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    #[derive(sqlx::Type)]
    #[repr(i8)]
    enum DrvBuildStateRepr {
        Queued = 0,
        Buildable = 1,
        Building = 7,
        CompletedSuccess = 42,
        CompletedFailure = -1,
        TransitiveFailure = -2,
        InterruptedOutOfMemory = -104,
        InterruptedTimeout = -120,
        InterruptedCancelled = -86,
        InterruptedProcessDeath = -66,
        InterruptedSchedulerDeath = -13,
        Blocked = 100,
    }

    impl From<&DrvBuildState> for DrvBuildStateRepr {
        fn from(value: &DrvBuildState) -> Self {
            match value {
                DrvBuildState::Queued => Self::Queued,
                DrvBuildState::Buildable => Self::Buildable,
                DrvBuildState::Building => Self::Building,
                DrvBuildState::Completed(DrvBuildResult::Success) => Self::CompletedSuccess,
                DrvBuildState::Completed(DrvBuildResult::Failure) => Self::CompletedFailure,
                DrvBuildState::TransitiveFailure => Self::TransitiveFailure,
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => {
                    Self::InterruptedOutOfMemory
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => {
                    Self::InterruptedTimeout
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
                    Self::InterruptedCancelled
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => {
                    Self::InterruptedProcessDeath
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => {
                    Self::InterruptedSchedulerDeath
                }
                DrvBuildState::Blocked => Self::Blocked,
            }
        }
    }

    impl From<DrvBuildStateRepr> for DrvBuildState {
        fn from(value: DrvBuildStateRepr) -> Self {
            match value {
                DrvBuildStateRepr::Queued => Self::Queued,
                DrvBuildStateRepr::Buildable => Self::Buildable,
                DrvBuildStateRepr::Building => Self::Building,
                DrvBuildStateRepr::CompletedSuccess => Self::Completed(DrvBuildResult::Success),
                DrvBuildStateRepr::CompletedFailure => Self::Completed(DrvBuildResult::Failure),
                DrvBuildStateRepr::TransitiveFailure => Self::TransitiveFailure,
                DrvBuildStateRepr::InterruptedOutOfMemory => {
                    Self::Interrupted(DrvBuildInterruptionKind::OutOfMemory)
                }
                DrvBuildStateRepr::InterruptedTimeout => {
                    Self::Interrupted(DrvBuildInterruptionKind::Timeout)
                }
                DrvBuildStateRepr::InterruptedCancelled => {
                    Self::Interrupted(DrvBuildInterruptionKind::Cancelled)
                }
                DrvBuildStateRepr::InterruptedProcessDeath => {
                    Self::Interrupted(DrvBuildInterruptionKind::ProcessDeath)
                }
                DrvBuildStateRepr::InterruptedSchedulerDeath => {
                    Self::Interrupted(DrvBuildInterruptionKind::SchedulerDeath)
                }
                DrvBuildStateRepr::Blocked => Self::Blocked,
            }
        }
    }

    impl<'q> Encode<'q, Sqlite> for DrvBuildState {
        fn encode_by_ref(
            &self,
            buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
        ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
            <DrvBuildStateRepr as Encode<'q, Sqlite>>::encode_by_ref(&self.into(), buf)
        }

        fn size_hint(&self) -> usize {
            <DrvBuildStateRepr as Encode<'q, Sqlite>>::size_hint(&self.into())
        }
    }

    impl<'r> Decode<'r, Sqlite> for DrvBuildState {
        fn decode(
            value: <Sqlite as sqlx::Database>::ValueRef<'r>,
        ) -> Result<Self, sqlx::error::BoxDynError> {
            Ok(<DrvBuildStateRepr as Decode<Sqlite>>::decode(value)?.into())
        }
    }

    impl Type<Sqlite> for DrvBuildState {
        fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
            <DrvBuildStateRepr as Type<Sqlite>>::type_info()
        }

        fn compatible(ty: &<Sqlite as sqlx::Database>::TypeInfo) -> bool {
            <DrvBuildStateRepr as Type<Sqlite>>::compatible(ty)
        }
    }
}

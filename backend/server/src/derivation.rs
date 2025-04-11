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
use std::num::NonZero;

/// Unique identifier for a derivation build attempt.
///
/// Combines the derivation identifier with a counter that keeps track of the number of build
/// attempts for that derivation.
#[derive(Debug)]
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
    pub build_attempt: NonZero<u32>,
}

/// Metadata about a derivation build.
///
/// This metadata is useful to reproduce a build on a different machine. Note that in general,
/// only builds that ended with a state of [`DrvBuildState::Completed`] can be reproduced.
#[derive(Debug)]
pub struct DrvBuildMetadata {
    /// The derivation build this metadata is associated with.
    pub build: DrvBuildId,

    /// The Git commit this derivation build is based upon.
    ///
    /// Note that this may not be the only commit that can produce this derivation. Because a
    /// derivation only needs to fully build once, later commits may still include this
    /// derivation but do not trigger a new build.
    pub commit: gix_hash::ObjectId,

    /// The Nix command that was used to build this derivation.
    pub build_command: DrvBuildCommand,
}

/// Command used to build the derivation.
#[derive(Debug)]
pub enum DrvBuildCommand {
    /// Nix 1.0 style command. Fields are TBD.
    Nix1,
}

/// Emitted whenever a derivation build's state changes.
#[derive(Debug)]
pub struct DrvBuildEvent {
    /// The derivation build this event is associated with.
    pub build: DrvBuildId,

    /// Globally monotonically increasing numeric event identifier. Use this to determine the order
    /// of events. Events with a higher id happened after events with a lower id.
    ///
    /// Since this id is global it can be used to retrieve the latest build state for a derivation
    /// without having to check the [`DrvBuildId::build_attempt`].
    pub event_id: u32,

    /// The build state this event propagates.
    pub state: DrvBuildState,

    /// The timestamp when this event happened.
    ///
    /// This timestamp only has second accuracy, which makes it unsuitable for sorting of build
    /// events. If for example the build queue is empty, it is not unlikely that a build is
    /// scheduled ([`DrvBuildState::Pending`]) and started ([`DrvBuildState::Building`]) within the
    /// same second.
    ///
    /// Using a higher accuracy timestamp would make it incompatible with SQlite's datetime
    /// functions which is also not desirable.
    ///
    /// Instead, use [`event_id`][Self::event_id] to determine the order of events.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Describes the possible states a derivation build can be in.
#[derive(Debug)]
pub enum DrvBuildState {
    /// Build is scheduled in a queue and has not yet started.
    Pending,
    /// Build is currently running.
    Building,
    /// Build has completed, either successfully or not.
    Completed {
        result: DrvBuildResult,

        // TBD
        build_log: (),
    },
    /// Build was interrupted before it could complete.
    Interrupted { reason: DrvBuildInterruptionKind },
}

/// Possible causes for why the derivation build was interrupted.
#[derive(Debug)]
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

/// The result of building a derivation.
///
/// In essence, this enum captures whether the status code returned by the build command was `0`
/// or not.
#[derive(Debug)]
pub enum DrvBuildResult {
    /// The derivation build successfully.
    Ok,
    /// The derivation failed to build.
    Error,
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
#[derive(Debug)]
pub struct DrvId(String);

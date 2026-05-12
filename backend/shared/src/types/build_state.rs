use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Describes the possible states a derivation build can be in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Derivation failed once and is waiting to be retried.
    ///
    /// The derivation was attempted to be built but failed. This is a second chance before
    /// marking it as permanently failed. Treated similar to Buildable - can be picked up by
    /// builders for a retry attempt. If this retry also fails, the derivation will be marked
    /// as Completed(Failure) and transitive failures will be propagated.
    FailedRetry,
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
    /// This build requires system features that no available builder provides.
    ///
    /// This is a terminal state. The derivation cannot be built until a builder with the
    /// required features is added to the system. All transitive dependants will be marked
    /// as having transitive failure.
    UnsatisfiableRequirements,
}

impl DrvBuildState {
    /// Check if this state represents a failure
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            DrvBuildState::Completed(DrvBuildResult::Failure)
                | DrvBuildState::TransitiveFailure
                | DrvBuildState::Interrupted(_)
                | DrvBuildState::UnsatisfiableRequirements
        )
    }

    /// Check if this state is terminal (build won't change from this state)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            DrvBuildState::Completed(_)
                | DrvBuildState::TransitiveFailure
                | DrvBuildState::Interrupted(_)
                | DrvBuildState::UnsatisfiableRequirements
        )
    }
}

impl FromStr for DrvBuildState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "queued" => Ok(DrvBuildState::Queued),
            "buildable" => Ok(DrvBuildState::Buildable),
            "failedretry" | "failed_retry" => Ok(DrvBuildState::FailedRetry),
            "building" => Ok(DrvBuildState::Building),
            "transitivefailure" | "transitive_failure" => Ok(DrvBuildState::TransitiveFailure),
            "blocked" => Ok(DrvBuildState::Blocked),
            "unsatisfiablerequirements" | "unsatisfiable_requirements" => {
                Ok(DrvBuildState::UnsatisfiableRequirements)
            },
            // Common shortcuts for completed states
            "success" => Ok(DrvBuildState::Completed(DrvBuildResult::Success)),
            "failure" => Ok(DrvBuildState::Completed(DrvBuildResult::Failure)),
            _ => Err(format!(
                "Invalid build state '{}'. Valid states: queued, buildable, failed_retry, \
                 building, success, failure, transitive_failure, blocked, \
                 unsatisfiable_requirements",
                s
            )),
        }
    }
}

/// The result of building a derivation.
///
/// In essence, this enum captures whether the status code returned by the build command was `0`
/// or not.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

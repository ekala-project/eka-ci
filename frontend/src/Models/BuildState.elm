module Models.BuildState exposing
    ( DrvBuildInterruptionKind(..)
    , DrvBuildResult(..)
    , DrvBuildState(..)
    , isTerminal
    , toString
    )

{-| Build state types matching the backend Rust enums
(`backend/server/src/db/model/build_event.rs`).

These types represent the various states a derivation build can be in during the CI process.

-}


{-| The state of a derivation build.
-}
type DrvBuildState
    = Queued
    | Buildable
    | FailedRetry
    | Building
    | Completed DrvBuildResult
    | TransitiveFailure
    | Interrupted DrvBuildInterruptionKind
    | Blocked
    | UnsatisfiableRequirements


{-| The result of a completed build.
-}
type DrvBuildResult
    = Success
    | Failure


{-| Reasons why a build was interrupted.
-}
type DrvBuildInterruptionKind
    = OutOfMemory
    | Timeout
    | Cancelled
    | ProcessDeath
    | SchedulerDeath


{-| Convert a build state to a short, sentence-case label.
-}
toString : DrvBuildState -> String
toString state =
    case state of
        Queued ->
            "Queued"

        Buildable ->
            "Buildable"

        FailedRetry ->
            "Retrying"

        Building ->
            "Building"

        Completed Success ->
            "Succeeded"

        Completed Failure ->
            "Failed"

        TransitiveFailure ->
            "Dependency failed"

        Interrupted OutOfMemory ->
            "Out of memory"

        Interrupted Timeout ->
            "Timed out"

        Interrupted Cancelled ->
            "Cancelled"

        Interrupted ProcessDeath ->
            "Process died"

        Interrupted SchedulerDeath ->
            "Scheduler died"

        Blocked ->
            "Blocked"

        UnsatisfiableRequirements ->
            "Unsatisfiable"


{-| Check if a build state is terminal (won't change anymore).
-}
isTerminal : DrvBuildState -> Bool
isTerminal state =
    case state of
        Completed _ ->
            True

        Interrupted _ ->
            True

        TransitiveFailure ->
            True

        UnsatisfiableRequirements ->
            True

        _ ->
            False

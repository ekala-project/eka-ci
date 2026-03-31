module Models.BuildState exposing
    ( DrvBuildInterruptionKind(..)
    , DrvBuildResult(..)
    , DrvBuildState(..)
    , isTerminal
    , toColor
    , toString
    )

{-| Build state types matching the backend Rust enums.

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


{-| Convert a build state to a human-readable string.
-}
toString : DrvBuildState -> String
toString state =
    case state of
        Queued ->
            "Queued"

        Buildable ->
            "Buildable"

        FailedRetry ->
            "Failed (Retrying)"

        Building ->
            "Building"

        Completed Success ->
            "Success"

        Completed Failure ->
            "Failed"

        TransitiveFailure ->
            "Blocked by Failure"

        Interrupted OutOfMemory ->
            "Out of Memory"

        Interrupted Timeout ->
            "Timeout"

        Interrupted Cancelled ->
            "Cancelled"

        Interrupted ProcessDeath ->
            "Process Died"

        Blocked ->
            "Blocked"


{-| Get a color for displaying the build state (for badges/UI).
Returns a CSS color class name.
-}
toColor : DrvBuildState -> String
toColor state =
    case state of
        Queued ->
            "gray"

        Buildable ->
            "blue"

        FailedRetry ->
            "orange"

        Building ->
            "yellow"

        Completed Success ->
            "green"

        Completed Failure ->
            "red"

        TransitiveFailure ->
            "dark-orange"

        Interrupted _ ->
            "purple"

        Blocked ->
            "gray"


{-| Check if a build state is terminal (won't change anymore).
-}
isTerminal : DrvBuildState -> Bool
isTerminal state =
    case state of
        Completed _ ->
            True

        Interrupted _ ->
            True

        _ ->
            False

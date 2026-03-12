module Components.StatusBadge exposing (view)

{-| Status badge component for displaying build states.

Displays a colored badge with text indicating the current build state.

-}

import Html exposing (Html, span, text)
import Html.Attributes exposing (class)
import Models.BuildState as BS


{-| View a status badge for a build state.

    view BS.Building
    --> <span class="status-badge status-building">BUILDING</span>

-}
view : BS.DrvBuildState -> Html msg
view state =
    let
        ( statusClass, statusText ) =
            case state of
                BS.Queued ->
                    ( "status-queued", "QUEUED" )

                BS.Buildable ->
                    ( "status-buildable", "BUILDABLE" )

                BS.FailedRetry ->
                    ( "status-failed-retry", "RETRY" )

                BS.Building ->
                    ( "status-building", "BUILDING" )

                BS.Completed BS.Success ->
                    ( "status-success", "SUCCESS" )

                BS.Completed BS.Failure ->
                    ( "status-failure", "FAILED" )

                BS.TransitiveFailure ->
                    ( "status-transitive-failure", "TRANSITIVE" )

                BS.Interrupted BS.OutOfMemory ->
                    ( "status-interrupted", "OOM" )

                BS.Interrupted BS.Timeout ->
                    ( "status-interrupted", "TIMEOUT" )

                BS.Interrupted BS.Cancelled ->
                    ( "status-interrupted", "CANCELLED" )

                BS.Interrupted BS.ProcessDeath ->
                    ( "status-interrupted", "DIED" )

                BS.Blocked ->
                    ( "status-blocked", "BLOCKED" )
    in
    span [ class ("status-badge " ++ statusClass) ]
        [ text statusText ]

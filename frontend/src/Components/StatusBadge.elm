module Components.StatusBadge exposing (view)

{-| Status badge for a derivation's build state.

The colour comes from the state's progress bucket (see
`Components.BuildProgress`), so badges, bars and filters always agree; the
label is the specific state ("Out of memory", "Timed out", …).

-}

import Components.BuildProgress as Progress
import Components.Ui as Ui
import Html exposing (Html)
import Models.BuildState as BS


view : BS.DrvBuildState -> Html msg
view state =
    let
        bucket =
            Progress.bucketOf state

        tone =
            Progress.bucketTone bucket
    in
    if bucket == Progress.Building then
        Ui.liveBadge tone (BS.toString state)

    else
        Ui.badge tone (BS.toString state)

module Components.BuildProgress exposing
    ( Bucket(..)
    , Counts
    , add
    , bar
    , bucketLabel
    , bucketOf
    , bucketRank
    , bucketTone
    , cell
    , countOf
    , empty
    , filterOrder
    , fromJobSet
    , fromPullRequest
    , fromStates
    , isFinished
    , overall
    , overallBadge
    , view
    )

{-| Build progress: one mapping from build states to display buckets, used
by every bar, legend, chip and status badge in the UI.

    Succeeded          Completed Success
    Failed             Completed Failure, Interrupted _, UnsatisfiableRequirements
    Dependency failed  TransitiveFailure, Blocked
    Retrying           FailedRetry
    Building           Building
    Pending            Queued, Buildable (and anything not yet counted)

-}

import Components.Ui as Ui exposing (Tone(..))
import Html exposing (Html, div, li, span, strong, text, ul)
import Html.Attributes exposing (attribute, class, style)
import Models.BuildState as BS exposing (DrvBuildState)


type Bucket
    = Succeeded
    | Failed
    | DepFailed
    | Retrying
    | Building
    | Pending


type alias Counts =
    { total : Int
    , succeeded : Int
    , failed : Int
    , depFailed : Int
    , retrying : Int
    , building : Int
    , pending : Int
    }


bucketOf : DrvBuildState -> Bucket
bucketOf state =
    case state of
        BS.Queued ->
            Pending

        BS.Buildable ->
            Pending

        BS.FailedRetry ->
            Retrying

        BS.Building ->
            Building

        BS.Completed BS.Success ->
            Succeeded

        BS.Completed BS.Failure ->
            Failed

        BS.Interrupted _ ->
            Failed

        BS.UnsatisfiableRequirements ->
            Failed

        BS.TransitiveFailure ->
            DepFailed

        BS.Blocked ->
            DepFailed


bucketLabel : Bucket -> String
bucketLabel bucket =
    case bucket of
        Succeeded ->
            "Succeeded"

        Failed ->
            "Failed"

        DepFailed ->
            "Dependency failed"

        Retrying ->
            "Retrying"

        Building ->
            "Building"

        Pending ->
            "Pending"


bucketTone : Bucket -> Tone
bucketTone bucket =
    case bucket of
        Succeeded ->
            Good

        Failed ->
            Critical

        DepFailed ->
            Serious

        Retrying ->
            Warn

        Building ->
            Info

        Pending ->
            Neutral


{-| Bar and legend order: done, then problems, then work still to do.
-}
barOrder : List Bucket
barOrder =
    [ Succeeded, Failed, DepFailed, Retrying, Building, Pending ]


{-| Filter and sort order: problems first, so failures are what you see.
-}
filterOrder : List Bucket
filterOrder =
    [ Failed, DepFailed, Retrying, Building, Pending, Succeeded ]


{-| Sort key putting problems first (see `filterOrder`).
-}
bucketRank : Bucket -> Int
bucketRank bucket =
    case bucket of
        Failed ->
            0

        DepFailed ->
            1

        Retrying ->
            2

        Building ->
            3

        Pending ->
            4

        Succeeded ->
            5


countOf : Bucket -> Counts -> Int
countOf bucket counts =
    case bucket of
        Succeeded ->
            counts.succeeded

        Failed ->
            counts.failed

        DepFailed ->
            counts.depFailed

        Retrying ->
            counts.retrying

        Building ->
            counts.building

        Pending ->
            counts.pending



-- CONSTRUCTORS


empty : Counts
empty =
    { total = 0
    , succeeded = 0
    , failed = 0
    , depFailed = 0
    , retrying = 0
    , building = 0
    , pending = 0
    }


{-| Combine counts, e.g. every job of a commit.
-}
add : Counts -> Counts -> Counts
add a b =
    { total = a.total + b.total
    , succeeded = a.succeeded + b.succeeded
    , failed = a.failed + b.failed
    , depFailed = a.depFailed + b.depFailed
    , retrying = a.retrying + b.retrying
    , building = a.building + b.building
    , pending = a.pending + b.pending
    }


type alias StateCounts a =
    { a
        | totalDrvs : Int
        , queuedDrvs : Int
        , buildableDrvs : Int
        , buildingDrvs : Int
        , completedSuccessDrvs : Int
        , completedFailureDrvs : Int
        , failedRetryDrvs : Int
        , transitiveFailureDrvs : Int
        , blockedDrvs : Int
        , interruptedDrvs : Int
    }


{-| From the per-state counts the API returns for a jobset. States the API
does not count separately (e.g. unsatisfiable) are shown as pending.
-}
fromJobSet : StateCounts a -> Counts
fromJobSet s =
    withRemainderPending s.totalDrvs
        { total = s.totalDrvs
        , succeeded = s.completedSuccessDrvs
        , failed = s.completedFailureDrvs + s.interruptedDrvs
        , depFailed = s.transitiveFailureDrvs + s.blockedDrvs
        , retrying = s.failedRetryDrvs
        , building = s.buildingDrvs
        , pending = s.queuedDrvs + s.buildableDrvs
        }


{-| Pull requests only report success / failure / retry counts; the rest is
still to do.
-}
fromPullRequest :
    { a
        | totalDrvs : Int
        , completedSuccessDrvs : Int
        , completedFailureDrvs : Int
        , failedRetryDrvs : Int
    }
    -> Counts
fromPullRequest pr =
    withRemainderPending pr.totalDrvs
        { total = pr.totalDrvs
        , succeeded = pr.completedSuccessDrvs
        , failed = pr.completedFailureDrvs
        , depFailed = 0
        , retrying = pr.failedRetryDrvs
        , building = 0
        , pending = 0
        }


fromStates : List DrvBuildState -> Counts
fromStates states =
    List.foldl (\state acc -> increment (bucketOf state) acc)
        { empty | total = List.length states }
        states


increment : Bucket -> Counts -> Counts
increment bucket c =
    case bucket of
        Succeeded ->
            { c | succeeded = c.succeeded + 1 }

        Failed ->
            { c | failed = c.failed + 1 }

        DepFailed ->
            { c | depFailed = c.depFailed + 1 }

        Retrying ->
            { c | retrying = c.retrying + 1 }

        Building ->
            { c | building = c.building + 1 }

        Pending ->
            { c | pending = c.pending + 1 }


withRemainderPending : Int -> Counts -> Counts
withRemainderPending total c =
    let
        counted =
            c.succeeded + c.failed + c.depFailed + c.retrying + c.building + c.pending
    in
    { c | pending = c.pending + max 0 (total - counted) }



-- STATUS


finished : Counts -> Int
finished c =
    c.succeeded + c.failed + c.depFailed


isFinished : Counts -> Bool
isFinished c =
    c.retrying + c.building + c.pending == 0


{-| The overall status of a set of builds. Any failure wins over progress,
so a failing PR never reads as "passed" while it is still building.
-}
overall : Counts -> ( Tone, String )
overall c =
    if c.total == 0 then
        ( Neutral, "No builds" )

    else if c.failed + c.depFailed > 0 then
        ( Critical, "Failing" )

    else if not (isFinished c) then
        ( Info, "In progress" )

    else
        ( Good, "Passed" )


overallBadge : Counts -> Html msg
overallBadge c =
    case overall c of
        ( Info, label ) ->
            Ui.liveBadge Info label

        ( tone, label ) ->
            Ui.badge tone label



-- VIEWS


percent : Int -> Int -> String
percent part whole =
    if whole <= 0 then
        "0%"

    else
        String.fromInt (round (toFloat part * 100 / toFloat whole)) ++ "%"


ariaSummary : Counts -> String
ariaSummary c =
    if c.total == 0 then
        "No derivations"

    else
        barOrder
            |> List.filter (\b -> countOf b c > 0)
            |> List.map (\b -> String.fromInt (countOf b c) ++ " " ++ String.toLower (bucketLabel b))
            |> String.join ", "
            |> (\parts -> parts ++ " of " ++ String.fromInt c.total ++ " derivations")


{-| A thin part-to-whole bar. Segment sizes follow the counts; values are
also printed by `legend` / `cell`, tooltips are only an enhancement.
-}
bar : Counts -> Html msg
bar c =
    if c.total == 0 then
        div [ class "segbar segbar-empty", attribute "role" "img", attribute "aria-label" "No derivations" ] []

    else
        div [ class "segbar", attribute "role" "img", attribute "aria-label" (ariaSummary c) ]
            (barOrder
                |> List.filter (\b -> countOf b c > 0)
                |> List.map (segment c)
            )


segment : Counts -> Bucket -> Html msg
segment c bucket =
    let
        n =
            countOf bucket c
    in
    span
        [ class ("seg seg-" ++ Ui.toneName (bucketTone bucket))
        , style "flex-grow" (String.fromInt n)
        , Ui.dataTip (bucketLabel bucket ++ ": " ++ String.fromInt n ++ " (" ++ percent n c.total ++ ")")
        ]
        []


legend : Counts -> Html msg
legend c =
    ul [ class "legend" ]
        (barOrder
            |> List.filter (\b -> countOf b c > 0)
            |> List.map
                (\b ->
                    li []
                        [ span [ class ("swatch seg-" ++ Ui.toneName (bucketTone b)), attribute "aria-hidden" "true" ] []
                        , text (bucketLabel b)
                        , span [ class "legend-value" ] [ text (String.fromInt (countOf b c)) ]
                        ]
                )
        )


{-| Full progress block: "N of M finished", the bar, and a legend.
-}
view : Counts -> Html msg
view c =
    div [ class "composition" ]
        [ div [ class "progress-summary" ]
            [ span []
                [ strong [] [ text (String.fromInt (finished c)) ]
                , text (" of " ++ String.fromInt c.total ++ " derivations finished")
                ]
            , span [] [ text (percent (finished c) c.total) ]
            ]
        , bar c
        , if c.total == 0 then
            text ""

          else
            legend c
        ]


{-| Compact progress for a table cell: bar plus a one-line count.
-}
cell : Counts -> Html msg
cell c =
    div []
        [ bar c
        , span [ class "small muted" ] [ text (cellSummary c) ]
        ]


cellSummary : Counts -> String
cellSummary c =
    let
        base =
            String.fromInt (finished c) ++ " / " ++ String.fromInt c.total

        problems =
            List.filterMap identity
                [ nonZero c.failed "failed"
                , nonZero c.depFailed "dep. failed"
                , nonZero c.building "building"
                ]
    in
    String.join " · " (base :: problems)


nonZero : Int -> String -> Maybe String
nonZero n label =
    if n > 0 then
        Just (String.fromInt n ++ " " ++ label)

    else
        Nothing

module Pages.Builds exposing
    ( Model
    , Msg
    , cleanup
    , init
    , subscriptions
    , update
    , view
    , websocketUpdate
    )

{-| Active builds: every job with work in flight, and what is building now.

Job progress updates live over the WebSocket (`allbuilds` subscription);
the list of building derivations is refreshed every few seconds because the
server does not push it.

-}

import Api.Api as Api
import Components.BuildProgress as Progress
import Components.Ui as Ui
import Dict exposing (Dict)
import Html exposing (Html, a, div, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href, scope, title)
import Http
import Models.BuildState as BS
import Models.Job exposing (BuildingDrv, JobDifference(..), JobSetDetails)
import Ports exposing (JobStatsUpdateEvent)
import Route
import Time


type alias Model =
    { apiBaseUrl : String
    , data : Data
    }


type Data
    = Loading
    | Loaded { jobs : Dict Int JobSetDetails, buildingDrvs : List BuildingDrv }
    | Failed Http.Error


type Msg
    = GotActiveBuilds (Result Http.Error Api.ActiveBuildsResponse)
    | Refresh


init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( { apiBaseUrl = apiBaseUrl, data = Loading }
    , Api.getActiveBuilds apiBaseUrl GotActiveBuilds
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotActiveBuilds (Ok response) ->
            ( { model
                | data =
                    Loaded
                        { jobs = Dict.fromList (List.map (\job -> ( job.jobsetId, job )) response.jobs)
                        , buildingDrvs = response.buildingDrvs
                        }
              }
            , Cmd.none
            )

        GotActiveBuilds (Err error) ->
            case model.data of
                Loaded _ ->
                    -- Keep showing the last good data if a background refresh fails.
                    ( model, Cmd.none )

                _ ->
                    ( { model | data = Failed error }, Cmd.none )

        Refresh ->
            ( model, Api.getActiveBuilds model.apiBaseUrl GotActiveBuilds )


subscriptions : Model -> Sub Msg
subscriptions _ =
    Time.every 10000 (\_ -> Refresh)


{-| Apply a live job stats update.
-}
websocketUpdate : JobStatsUpdateEvent -> Model -> Model
websocketUpdate event model =
    case model.data of
        Loaded data ->
            { model | data = Loaded { data | jobs = Dict.update event.jobsetId (Maybe.map (applyStats event)) data.jobs } }

        _ ->
            model


applyStats : JobStatsUpdateEvent -> JobSetDetails -> JobSetDetails
applyStats event job =
    { job
        | totalDrvs = event.totalDrvs
        , queuedDrvs = event.queuedDrvs
        , buildableDrvs = event.buildableDrvs
        , buildingDrvs = event.buildingDrvs
        , completedSuccessDrvs = event.completedSuccessDrvs
        , completedFailureDrvs = event.completedFailureDrvs
        , failedRetryDrvs = event.failedRetryDrvs
        , transitiveFailureDrvs = event.transitiveFailureDrvs
        , blockedDrvs = event.blockedDrvs
        , interruptedDrvs = event.interruptedDrvs
    }


{-| Unsubscribe from the WebSocket when leaving the page.
-}
cleanup : Model -> Cmd msg
cleanup _ =
    Ports.websocketOut (Ports.encodeUnsubscribeMessage "allbuilds" "all")



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ Ui.simpleHead "Active builds" "Jobs with derivations still queued or building. Progress updates live."
        , case model.data of
            Loading ->
                Ui.loading "Loading active builds…"

            Failed error ->
                Ui.httpError "Could not load active builds." error

            Loaded data ->
                viewLoaded data
        ]


viewLoaded : { jobs : Dict Int JobSetDetails, buildingDrvs : List BuildingDrv } -> Html Msg
viewLoaded data =
    let
        building =
            List.filter (\drv -> drv.buildState == BS.Building) data.buildingDrvs

        jobs =
            List.reverse (Dict.values data.jobs)

        total =
            List.foldl (Progress.add << Progress.fromJobSet) Progress.empty jobs
    in
    if List.isEmpty jobs && List.isEmpty building then
        Ui.emptyState "Nothing is building" "New pushes and pull requests will show up here while they build."

    else
        div []
            [ div [ class "tiles" ]
                [ tile "Building now" (List.length building) Ui.Info
                , tile "Active jobs" (List.length jobs) Ui.Neutral
                , tile "Pending" (total.pending + total.retrying) Ui.Neutral
                , tile "Failed so far" (total.failed + total.depFailed) Ui.Critical
                ]
            , Ui.panel "Building now"
                [ span [ class "small muted" ] [ text "Refreshes every 10 seconds" ] ]
                [ if List.isEmpty building then
                    Ui.emptyNote "No derivation is building right now."

                  else
                    Ui.tableWrap (buildingTable building)
                ]
            , Ui.panel "Jobs"
                []
                [ if List.isEmpty jobs then
                    Ui.emptyNote "No jobs are in progress."

                  else
                    Ui.tableWrap (jobTable jobs)
                ]
            ]


tile : String -> Int -> Ui.Tone -> Html msg
tile label value tone =
    div [ class "tile" ]
        [ span [ class "tile-label" ]
            [ span [ class ("dot " ++ Ui.toneClass tone), Html.Attributes.attribute "aria-hidden" "true" ] []
            , text label
            ]
        , span [ class "tile-value" ] [ text (String.fromInt value) ]
        ]


buildingTable : List BuildingDrv -> Html Msg
buildingTable drvs =
    table [ class "table compact" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Derivation" ]
                , th [ scope "col" ] [ text "System" ]
                , th [ scope "col", class "hide-sm" ] [ text "Notes" ]
                ]
            ]
        , tbody [] (List.map buildingRow drvs)
        ]


buildingRow : BuildingDrv -> Html Msg
buildingRow drv =
    tr []
        [ td []
            [ a [ class "cell-main", href (Route.toHref (Route.Drv drv.drvPath)), title ("/nix/store/" ++ drv.drvPath) ]
                [ text (Maybe.withDefault (Ui.drvName drv.drvPath) drv.name) ]
            ]
        , td [ class "mono small nowrap" ] [ text drv.system ]
        , td [ class "hide-sm" ] [ span [ class "tags" ] (List.filterMap identity [ fodTag drv.isFod, Maybe.map differenceTag drv.difference ]) ]
        ]


fodTag : Bool -> Maybe (Html msg)
fodTag isFod =
    if isFod then
        Just (span [ class "tag", title "Fixed-output derivation (fetches sources)" ] [ text "FOD" ])

    else
        Nothing


differenceTag : JobDifference -> Html msg
differenceTag difference =
    case difference of
        New ->
            span [ class "tag", title "A job that is new in this change" ] [ text "New job" ]

        Changed ->
            span [ class "tag", title "A job whose derivation changed" ] [ text "Changed job" ]

        Removed ->
            span [ class "tag" ] [ text "Removed job" ]


jobTable : List JobSetDetails -> Html Msg
jobTable jobs =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Job" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col" ] [ text "Progress" ]
                ]
            ]
        , tbody [] (List.map jobRow jobs)
        ]


jobRow : JobSetDetails -> Html Msg
jobRow job =
    let
        counts =
            Progress.fromJobSet job
    in
    tr []
        [ td []
            [ a [ class "cell-main", href (Route.toHref (Route.Job job.jobsetId)) ] [ text job.jobName ]
            , span [ class "cell-sub" ]
                [ a [ href (Route.toHref (Route.Repository job.owner job.repoName)) ] [ text (job.owner ++ "/" ++ job.repoName) ]
                , text " at "
                , a [ class "mono", href (Route.toHref (Route.Commit job.sha)), title job.sha ] [ text (Ui.shortSha job.sha) ]
                ]
            ]
        , td [] [ Progress.overallBadge counts ]
        , td [ class "progress-cell" ] [ Progress.cell counts ]
        ]

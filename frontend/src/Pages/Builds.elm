module Pages.Builds exposing
    ( Model
    , Msg
    , cleanup
    , init
    , update
    , view
    , websocketUpdate
    )

{-| Active Builds page displaying all in-progress builds.

Shows all jobs with queued, buildable, or building drvs, with real-time
WebSocket updates for build progress.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.ProgressBar as ProgressBar
import Components.StatusBadge as StatusBadge
import Dict exposing (Dict)
import Html exposing (Html, a, div, h1, h2, h3, p, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href)
import Http
import Models.BuildState as BS
import Models.Job exposing (BuildingDrv, JobSetDetails)
import Ports exposing (JobStatsUpdateEvent)
import Route


{-| Page model with active builds data.
-}
type Model
    = Loading String
    | Loaded LoadedData
    | Failed Http.Error


{-| Loaded data with active jobs and building drvs.
-}
type alias LoadedData =
    { jobs : Dict Int JobSetDetails
    , buildingDrvs : List BuildingDrv
    , apiBaseUrl : String
    }


{-| Page messages.
-}
type Msg
    = GotActiveBuilds (Result Http.Error Api.ActiveBuildsResponse)


{-| Initialize the page.
-}
init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( Loading apiBaseUrl
    , Api.getActiveBuilds apiBaseUrl GotActiveBuilds
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotActiveBuilds result ->
            case ( result, model ) of
                ( Ok response, Loading apiBaseUrl ) ->
                    let
                        jobsDict =
                            response.jobs
                                |> List.map (\job -> ( job.jobsetId, job ))
                                |> Dict.fromList
                    in
                    ( Loaded
                        { jobs = jobsDict
                        , buildingDrvs = response.buildingDrvs
                        , apiBaseUrl = apiBaseUrl
                        }
                    , Cmd.none
                    )

                ( Ok response, _ ) ->
                    -- Shouldn't happen, but handle gracefully
                    let
                        jobsDict =
                            response.jobs
                                |> List.map (\job -> ( job.jobsetId, job ))
                                |> Dict.fromList
                    in
                    ( Loaded
                        { jobs = jobsDict
                        , buildingDrvs = response.buildingDrvs
                        , apiBaseUrl = ""
                        }
                    , Cmd.none
                    )

                ( Err err, _ ) ->
                    ( Failed err, Cmd.none )


{-| Handle WebSocket updates for job stats.
-}
websocketUpdate : JobStatsUpdateEvent -> Model -> Model
websocketUpdate event model =
    case model of
        Loaded data ->
            let
                updatedJob =
                    Dict.get event.jobsetId data.jobs
                        |> Maybe.map
                            (\job ->
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
                            )

                newJobs =
                    case updatedJob of
                        Just job ->
                            Dict.insert event.jobsetId job data.jobs

                        Nothing ->
                            data.jobs
            in
            Loaded { data | jobs = newJobs }

        _ ->
            model


{-| Cleanup when leaving the page (unsubscribe from WebSocket).
-}
cleanup : Model -> Cmd msg
cleanup model =
    -- Unsubscribe from "all_builds"
    Ports.websocketOut (Ports.encodeUnsubscribeMessage "allbuilds" "all")


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            Loader.view

        Failed err ->
            div [ class "container mx-auto px-4 py-8" ]
                [ h1 [ class "text-3xl font-bold mb-6" ] [ text "Active Builds" ]
                , ErrorView.viewHttp err
                ]

        Loaded data ->
            div [ class "container mx-auto px-4 py-8" ]
                [ h1 [ class "text-3xl font-bold mb-6" ] [ text "Active Builds" ]
                , if Dict.isEmpty data.jobs then
                    div [ class "text-center py-12" ]
                        [ p [ class "text-gray-500 text-lg" ] [ text "No active builds at the moment" ]
                        ]

                  else
                    div [ class "space-y-6" ]
                        (data.jobs
                            |> Dict.values
                            |> List.map (viewJob data.buildingDrvs)
                        )
                ]


{-| View a single job with its progress and building drvs.
-}
viewJob : List BuildingDrv -> JobSetDetails -> Html Msg
viewJob buildingDrvs job =
    let
        segments =
            ProgressBar.fromJobStats
                { completed = job.completedSuccessDrvs
                , failed = job.completedFailureDrvs + job.failedRetryDrvs
                , building = job.buildingDrvs
                , queued = job.queuedDrvs + job.buildableDrvs
                }
    in
    div [ class "bg-white rounded-lg shadow-sm border border-gray-200 p-6" ]
        [ -- Job header with repo/commit/job info
          div [ class "mb-4" ]
            [ h2 [ class "text-xl font-semibold mb-2" ]
                [ a
                    [ href (Route.toHref (Route.Job job.jobsetId))
                    , class "text-blue-600 hover:text-blue-800 hover:underline"
                    ]
                    [ text (job.owner ++ "/" ++ job.repoName ++ " - " ++ job.jobName) ]
                ]
            , p [ class "text-sm text-gray-600" ]
                [ text "Commit: "
                , a
                    [ href (Route.toHref (Route.Commit job.sha))
                    , class "font-mono text-blue-600 hover:underline"
                    ]
                    [ text (String.left 7 job.sha) ]
                ]
            ]

        -- Stats summary
        , div [ class "mb-4 flex gap-4 text-sm" ]
            [ div []
                [ span [ class "text-gray-600" ] [ text "Queued: " ]
                , span [ class "font-semibold" ] [ text (String.fromInt job.queuedDrvs) ]
                ]
            , div []
                [ span [ class "text-gray-600" ] [ text "Buildable: " ]
                , span [ class "font-semibold" ] [ text (String.fromInt job.buildableDrvs) ]
                ]
            , div []
                [ span [ class "text-gray-600" ] [ text "Building: " ]
                , span [ class "font-semibold text-blue-600" ] [ text (String.fromInt job.buildingDrvs) ]
                ]
            ]

        -- Progress bar
        , div [ class "mb-4" ]
            [ ProgressBar.view job.totalDrvs segments ]

        -- Building drvs table (only show if there are building drvs)
        , if job.buildingDrvs > 0 then
            div [ class "mt-4" ]
                [ h3 [ class "text-lg font-semibold mb-2" ] [ text "Building Derivations" ]
                , table [ class "min-w-full divide-y divide-gray-200" ]
                    [ thead [ class "bg-gray-50" ]
                        [ tr []
                            [ th [ class "px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider" ]
                                [ text "Name" ]
                            , th [ class "px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider" ]
                                [ text "System" ]
                            , th [ class "px-4 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider" ]
                                [ text "Status" ]
                            ]
                        ]
                    , tbody [ class "bg-white divide-y divide-gray-200" ]
                        (buildingDrvs
                            |> List.filter (\drv -> drv.buildState == BS.Building)
                            |> List.map viewBuildingDrv
                        )
                    ]
                ]

          else
            text ""
        ]


{-| View a single building drv row.
-}
viewBuildingDrv : BuildingDrv -> Html Msg
viewBuildingDrv drv =
    let
        displayName =
            case drv.name of
                Just name ->
                    name

                Nothing ->
                    -- Show last part of drv path for intermediate dependencies
                    drv.drvPath
                        |> String.split "/"
                        |> List.reverse
                        |> List.head
                        |> Maybe.withDefault drv.drvPath
    in
    tr []
        [ td [ class "px-4 py-2 whitespace-nowrap" ]
            [ a
                [ href (Route.toHref (Route.Drv drv.drvPath))
                , class "text-blue-600 hover:underline font-medium"
                ]
                [ text displayName ]
            ]
        , td [ class "px-4 py-2 whitespace-nowrap text-sm text-gray-500" ]
            [ text drv.system ]
        , td [ class "px-4 py-2 whitespace-nowrap" ]
            [ StatusBadge.view drv.buildState ]
        ]

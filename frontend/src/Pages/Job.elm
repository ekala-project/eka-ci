module Pages.Job exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Job details page showing jobset information and all derivations.

Displays comprehensive information about a specific job including:
- Job metadata (SHA, name, repository)
- Overall progress
- List of all derivations with their states
- Real-time updates via WebSocket

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.ProgressBar as ProgressBar
import Components.StatusBadge as StatusBadge
import Html exposing (Html, a, code, div, h1, h2, p, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href)
import Http
import Models.BuildState as BS
import Models.Job exposing (JobSetDetails, JobSetDrv)
import Route


{-| Page model.
-}
type Model
    = Loading Int
    | Loaded JobData
    | Failed Http.Error


{-| Job data combining details and derivations.
-}
type alias JobData =
    { jobsetId : Int
    , details : JobSetDetails
    , drvs : List JobSetDrv
    }


{-| Page messages.
-}
type Msg
    = GotJobSetDetails (Result Http.Error JobSetDetails)
    | GotJobSetDrvs (Result Http.Error (List JobSetDrv))


{-| Initialize the page with a jobset ID.
-}
init : Int -> ( Model, Cmd Msg )
init jobsetId =
    ( Loading jobsetId
    , Cmd.batch
        [ Api.getJobSetDetails jobsetId GotJobSetDetails
        , Api.getJobSetDrvs jobsetId GotJobSetDrvs
        ]
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotJobSetDetails result ->
            case result of
                Ok details ->
                    case model of
                        Loading jobsetId ->
                            -- Wait for drvs to load too
                            ( Loaded
                                { jobsetId = jobsetId
                                , details = details
                                , drvs = []
                                }
                            , Cmd.none
                            )

                        Loaded data ->
                            ( Loaded { data | details = details }
                            , Cmd.none
                            )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )

        GotJobSetDrvs result ->
            case result of
                Ok drvs ->
                    case model of
                        Loaded data ->
                            ( Loaded { data | drvs = drvs }
                            , Cmd.none
                            )

                        Loading jobsetId ->
                            -- Details haven't loaded yet, create partial state
                            ( model, Cmd.none )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading jobsetId ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text ("Job #" ++ String.fromInt jobsetId) ]
                , Loader.viewWithMessage "Loading job details..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Job" ]
                , ErrorView.viewHttp error
                ]

        Loaded data ->
            viewJobDetails data


{-| View job details.
-}
viewJobDetails : JobData -> Html Msg
viewJobDetails data =
    div [ class "pa4" ]
        [ -- Header
          viewJobHeader data.details

        -- Progress
        , viewProgress data.details

        -- Derivations table
        , div [ class "mt4" ]
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Derivations" ]
            , viewDrvTable data.drvs
            ]
        ]


{-| View job header with metadata.
-}
viewJobHeader : JobSetDetails -> Html Msg
viewJobHeader details =
    div [ class "mb4" ]
        [ h1 [ class "f2 fw6 mb2" ]
            [ text details.jobName ]
        , div [ class "flex items-center gray f6" ]
            [ a
                [ href (Route.toHref (Route.Repository details.owner details.repoName))
                , class "link blue hover-dark-blue mr3"
                ]
                [ text (details.owner ++ "/" ++ details.repoName) ]
            , span [ class "mr3" ] [ text "•" ]
            , a
                [ href (Route.toHref (Route.Commit details.sha))
                , class "link blue hover-dark-blue monospace"
                ]
                [ text (String.left 8 details.sha) ]
            ]
        ]


{-| View overall progress bar.
-}
viewProgress : JobSetDetails -> Html Msg
viewProgress details =
    let
        segments =
            ProgressBar.fromJobStats
                { completed = details.completedDrvs
                , failed = details.failedDrvs + details.transitiveFailureDrvs
                , building = details.buildingDrvs
                , queued = details.queuedDrvs + details.buildableDrvs
                }
    in
    div [ class "card pa3" ]
        [ div [ class "mb3 flex items-center justify-between" ]
            [ h2 [ class "f5 fw6 ma0" ]
                [ text "Progress" ]
            , p [ class "f6 gray ma0" ]
                [ text
                    (String.fromInt details.completedDrvs
                        ++ " / "
                        ++ String.fromInt details.totalDrvs
                        ++ " completed"
                    )
                ]
            ]
        , ProgressBar.view details.totalDrvs segments

        -- Stats
        , div [ class "flex justify-around mt3 f7 gray" ]
            [ viewStat "Queued" details.queuedDrvs
            , viewStat "Buildable" details.buildableDrvs
            , viewStat "Building" details.buildingDrvs
            , viewStat "Completed" details.completedDrvs
            , viewStat "Failed" details.failedDrvs
            , viewStat "Transitive" details.transitiveFailureDrvs
            ]
        ]


{-| View a single stat.
-}
viewStat : String -> Int -> Html Msg
viewStat label count =
    div [ class "tc" ]
        [ div [ class "f5 fw6" ] [ text (String.fromInt count) ]
        , div [] [ text label ]
        ]


{-| View derivations table.
-}
viewDrvTable : List JobSetDrv -> Html Msg
viewDrvTable drvs =
    if List.isEmpty drvs then
        div [ class "card pa4 tc" ]
            [ p [ class "gray" ] [ text "No derivations found" ] ]

    else
        div [ class "card" ]
            [ table [ class "table w-100" ]
                [ thead []
                    [ tr []
                        [ th [] [ text "Name" ]
                        , th [] [ text "System" ]
                        , th [] [ text "Status" ]
                        , th [] [ text "Type" ]
                        , th [] [ text "Path" ]
                        ]
                    ]
                , tbody []
                    (List.map viewDrvRow drvs)
                ]
            ]


{-| View a single derivation row.
-}
viewDrvRow : JobSetDrv -> Html Msg
viewDrvRow drv =
    tr []
        [ td []
            [ a
                [ href (Route.toHref (Route.Drv drv.drvPath))
                , class "link blue hover-dark-blue"
                ]
                [ text drv.name ]
            ]
        , td [ class "gray f7" ]
            [ text drv.system ]
        , td []
            [ StatusBadge.view drv.buildState ]
        , td [ class "f7" ]
            [ if drv.isFod then
                text "FOD"

              else if drv.preferLocalBuild then
                text "Local"

              else
                text "-"
            ]
        , td [ class "monospace f7 gray truncate-path" ]
            [ text drv.drvPath ]
        ]

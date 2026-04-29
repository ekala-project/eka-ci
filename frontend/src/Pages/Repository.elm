module Pages.Repository exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Repository details page.

Shows information about a specific repository and its recent builds.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.ProgressBar as ProgressBar
import Components.StatusBadge as StatusBadge
import Html exposing (Html, a, button, div, h1, h2, h3, p, span, text)
import Html.Attributes exposing (class, href, title)
import Html.Events
import Http
import Models.Repository exposing (Repository, RepositoryJobSet, SortOrder(..))
import Route


{-| Page model.
-}
type Model
    = Loading String String String
    | LoadedRepository Repository JobsetsData
    | Failed Http.Error


type JobsetsData
    = LoadingJobsets
    | LoadedJobsets (List RepositoryJobSet) SortOrder
    | FailedJobsets Http.Error


{-| Page messages.
-}
type Msg
    = GotRepository (Result Http.Error Repository)
    | GotJobsets (Result Http.Error (List RepositoryJobSet))
    | ToggleSortOrder


{-| Initialize the page with owner and repository name.
-}
init : String -> String -> String -> ( Model, Cmd Msg )
init apiBaseUrl owner repoName =
    ( Loading apiBaseUrl owner repoName
    , Cmd.batch
        [ Api.getRepository apiBaseUrl owner repoName GotRepository
        , Api.getRepositoryJobsets apiBaseUrl owner repoName 50 True GotJobsets
        ]
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotRepository result ->
            case ( result, model ) of
                ( Ok repo, Loading apiBaseUrl owner repoName ) ->
                    ( LoadedRepository repo LoadingJobsets, Cmd.none )

                ( Ok repo, LoadedRepository _ jobsetsData ) ->
                    ( LoadedRepository repo jobsetsData, Cmd.none )

                ( Ok repo, Failed _ ) ->
                    -- Received repository after error, reset to loading state
                    ( LoadedRepository repo LoadingJobsets, Cmd.none )

                ( Err error, _ ) ->
                    ( Failed error, Cmd.none )

        GotJobsets result ->
            case ( result, model ) of
                ( Ok jobsets, LoadedRepository repo _ ) ->
                    ( LoadedRepository repo (LoadedJobsets jobsets DateDesc), Cmd.none )

                ( Ok jobsets, Loading apiBaseUrl owner repoName ) ->
                    -- Jobsets loaded before repo info, just wait for repo
                    ( model, Cmd.none )

                ( Err error, LoadedRepository repo _ ) ->
                    ( LoadedRepository repo (FailedJobsets error), Cmd.none )

                ( Err error, Loading _ _ _ ) ->
                    ( Failed error, Cmd.none )

                ( _, _ ) ->
                    ( model, Cmd.none )

        ToggleSortOrder ->
            case model of
                LoadedRepository repo (LoadedJobsets jobsets sortOrder) ->
                    let
                        newSortOrder =
                            case sortOrder of
                                DateDesc ->
                                    DateAsc

                                DateAsc ->
                                    DateDesc

                        sortedJobsets =
                            case newSortOrder of
                                DateDesc ->
                                    List.reverse jobsets

                                DateAsc ->
                                    List.reverse jobsets
                    in
                    ( LoadedRepository repo (LoadedJobsets sortedJobsets newSortOrder), Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading _ owner repoName ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text (owner ++ "/" ++ repoName) ]
                , Loader.viewWithMessage "Loading repository..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Repository" ]
                , ErrorView.viewHttp error
                ]

        LoadedRepository repo jobsetsData ->
            viewRepositoryDetails repo jobsetsData


{-| View repository details with jobsets.
-}
viewRepositoryDetails : Repository -> JobsetsData -> Html Msg
viewRepositoryDetails repo jobsetsData =
    div [ class "pa4" ]
        [ -- Header
          h1 [ class "f2 fw6 mb2" ]
            [ text (repo.owner ++ "/" ++ repo.repoName) ]

        -- Metadata
        , div [ class "gray f6 mb4" ]
            [ text ("Installation ID: " ++ String.fromInt repo.installationId) ]

        -- Jobsets section
        , viewJobsetsSection repo jobsetsData
        ]


{-| View the jobsets section.
-}
viewJobsetsSection : Repository -> JobsetsData -> Html Msg
viewJobsetsSection repo jobsetsData =
    div []
        [ h2 [ class "f3 fw6 mb3" ]
            [ text "Recent Builds" ]
        , case jobsetsData of
            LoadingJobsets ->
                Loader.viewWithMessage "Loading recent builds..."

            FailedJobsets error ->
                ErrorView.viewHttp error

            LoadedJobsets jobsets sortOrder ->
                if List.isEmpty jobsets then
                    p [ class "gray" ]
                        [ text "No builds found for this repository." ]

                else
                    div []
                        [ viewSortControls sortOrder
                        , div [ class "mt3" ]
                            (List.map viewJobsetCard jobsets)
                        ]
        ]


{-| View sort order controls.
-}
viewSortControls : SortOrder -> Html Msg
viewSortControls sortOrder =
    div [ class "flex items-center mb3 pb2 bb b--black-10" ]
        [ span [ class "gray f6 mr2" ] [ text "Sort by:" ]
        , button
            [ class "link dim blue pointer bg-transparent bn f6"
            , Html.Events.onClick ToggleSortOrder
            ]
            [ text <|
                case sortOrder of
                    DateDesc ->
                        "Newest first ↓"

                    DateAsc ->
                        "Oldest first ↑"
            ]
        ]


{-| View a single jobset card.
-}
viewJobsetCard : RepositoryJobSet -> Html Msg
viewJobsetCard jobset =
    let
        shortSha =
            String.left 7 jobset.sha

        totalCompleted =
            jobset.completedSuccessDrvs + jobset.completedFailureDrvs

        progress =
            if jobset.totalDrvs > 0 then
                toFloat totalCompleted / toFloat jobset.totalDrvs

            else
                0

        isDone =
            totalCompleted == jobset.totalDrvs

        hasFailures =
            jobset.completedFailureDrvs > 0 || jobset.transitiveFailureDrvs > 0
    in
    div [ class "card pa3 mb3" ]
        [ -- Header with job name and SHA
          div [ class "flex items-center justify-between mb3" ]
            [ div []
                [ h3 [ class "f4 fw6 ma0" ]
                    [ text (jobset.jobName ++ " @ " ++ shortSha) ]
                , div [ class "f6 gray mt1" ]
                    [ text ("Jobset ID: " ++ String.fromInt jobset.jobsetId) ]
                ]
            , div [ class "flex gap2" ]
                [ a
                    [ class "link dim blue"
                    , href (Route.toHref (Route.Job jobset.jobsetId))
                    , title "View jobset details"
                    ]
                    [ text "View Details" ]
                ]
            ]

        -- Progress bar
        , ProgressBar.view jobset.totalDrvs
            (ProgressBar.fromJobStats
                { completed = jobset.completedSuccessDrvs
                , failed = jobset.completedFailureDrvs + jobset.transitiveFailureDrvs
                , building = jobset.buildingDrvs
                , queued = jobset.queuedDrvs + jobset.buildableDrvs
                }
            )

        -- Build stats summary
        , div [ class "flex flex-wrap gap3 mt3 f6" ]
            [ viewStat "Total" (String.fromInt jobset.totalDrvs) "gray"
            , viewStat "Queued" (String.fromInt jobset.queuedDrvs) "gray"
            , viewStat "Building" (String.fromInt jobset.buildingDrvs) "blue"
            , viewStat "Succeeded" (String.fromInt jobset.completedSuccessDrvs) "green"
            , viewStat "Failed" (String.fromInt (jobset.completedFailureDrvs + jobset.transitiveFailureDrvs)) "red"
            ]

        -- Change summary preview
        , viewChangeSummary jobset
        ]


{-| View a statistic.
-}
viewStat : String -> String -> String -> Html msg
viewStat label value color =
    div [ class "flex items-center" ]
        [ span [ class "gray mr1" ] [ text (label ++ ":") ]
        , span [ class ("fw6 " ++ color) ] [ text value ]
        ]


{-| View change summary information.
-}
viewChangeSummary : RepositoryJobSet -> Html Msg
viewChangeSummary jobset =
    let
        hasChanges =
            jobset.newJobs > 0 || jobset.changedJobs > 0 || jobset.removedJobs > 0

        totalChanges =
            jobset.newJobs + jobset.changedJobs
    in
    if hasChanges then
        div [ class "mt3 pt3 bt b--black-10" ]
            [ div [ class "f6 gray mb2" ] [ text "Package Changes:" ]
            , div [ class "flex flex-wrap gap3 f6" ]
                [ if jobset.newJobs > 0 then
                    span [ class "green" ]
                        [ text ("📦 " ++ String.fromInt jobset.newJobs ++ " added") ]

                  else
                    text ""
                , if jobset.removedJobs > 0 then
                    span [ class "orange" ]
                        [ text ("📦 " ++ String.fromInt jobset.removedJobs ++ " removed") ]

                  else
                    text ""
                , if jobset.changedJobs > 0 then
                    span [ class "blue" ]
                        [ text ("🔄 " ++ String.fromInt jobset.changedJobs ++ " changed") ]

                  else
                    text ""
                , if totalChanges > 0 then
                    span [ class "gray" ]
                        [ text ("🔨 Rebuilds: ~" ++ String.fromInt totalChanges ++ " packages") ]

                  else
                    text ""
                ]
            ]

    else
        text ""

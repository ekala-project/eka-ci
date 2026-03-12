module Pages.Commit exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Commit page displaying jobs for a commit with real-time updates.

Shows all jobs (builds) associated with a specific commit SHA with:
- Rich commit header
- Expandable job sections
- Progress bars for each job
- Real-time WebSocket updates

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.ProgressBar as ProgressBar
import Html exposing (Html, a, button, code, div, h1, h2, h3, p, span, text)
import Html.Attributes exposing (class, href)
import Html.Events exposing (onClick, stopPropagationOn)
import Http
import Json.Decode
import Models.Job exposing (CommitJob, JobSetDetails)
import Route
import Set exposing (Set)


{-| Page model with expandable job sections.
-}
type Model
    = Loading String
    | Loaded LoadedData
    | Failed Http.Error


{-| Loaded data with jobs and expansion state.
-}
type alias LoadedData =
    { sha : String
    , jobs : List CommitJob
    , expandedJobs : Set Int
    , jobDetails : List ( Int, Maybe JobSetDetails )
    }


{-| Page messages including real-time updates.
-}
type Msg
    = GotCommitJobs (Result Http.Error (List CommitJob))
    | ToggleJobExpanded Int
    | GotJobDetails Int (Result Http.Error JobSetDetails)


{-| Initialize the page with a commit SHA.
-}
init : String -> ( Model, Cmd Msg )
init sha =
    ( Loading sha
    , Api.getCommitJobs sha GotCommitJobs
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotCommitJobs result ->
            case result of
                Ok jobs ->
                    case model of
                        Loading sha ->
                            ( Loaded
                                { sha = sha
                                , jobs = jobs
                                , expandedJobs = Set.empty
                                , jobDetails = List.map (\job -> ( job.jobsetId, Nothing )) jobs
                                }
                            , Cmd.none
                            )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )

        ToggleJobExpanded jobsetId ->
            case model of
                Loaded data ->
                    let
                        newExpanded =
                            if Set.member jobsetId data.expandedJobs then
                                Set.remove jobsetId data.expandedJobs

                            else
                                Set.insert jobsetId data.expandedJobs

                        -- Load job details if we're expanding and don't have them
                        shouldLoad =
                            not (Set.member jobsetId data.expandedJobs)
                                && not (hasJobDetails jobsetId data.jobDetails)

                        cmd =
                            if shouldLoad then
                                Api.getJobSetDetails jobsetId (GotJobDetails jobsetId)

                            else
                                Cmd.none
                    in
                    ( Loaded { data | expandedJobs = newExpanded }
                    , cmd
                    )

                _ ->
                    ( model, Cmd.none )

        GotJobDetails jobsetId result ->
            case model of
                Loaded data ->
                    case result of
                        Ok details ->
                            let
                                updatedDetails =
                                    updateJobDetails jobsetId (Just details) data.jobDetails
                            in
                            ( Loaded { data | jobDetails = updatedDetails }
                            , Cmd.none
                            )

                        Err _ ->
                            -- Keep the Nothing entry, don't retry
                            ( model, Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| Check if we have job details for a jobset ID.
-}
hasJobDetails : Int -> List ( Int, Maybe JobSetDetails ) -> Bool
hasJobDetails jobsetId details =
    List.any (\( id, maybeDetails ) -> id == jobsetId && maybeDetails /= Nothing) details


{-| Update job details in the list.
-}
updateJobDetails : Int -> Maybe JobSetDetails -> List ( Int, Maybe JobSetDetails ) -> List ( Int, Maybe JobSetDetails )
updateJobDetails jobsetId newDetails detailsList =
    List.map
        (\( id, existing ) ->
            if id == jobsetId then
                ( id, newDetails )

            else
                ( id, existing )
        )
        detailsList


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading sha ->
            div [ class "pa4" ]
                [ viewCommitHeader sha Nothing Nothing
                , Loader.viewWithMessage "Loading jobs..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Commit" ]
                , ErrorView.viewHttp error
                ]

        Loaded data ->
            if List.isEmpty data.jobs then
                div [ class "pa4" ]
                    [ viewCommitHeader data.sha Nothing Nothing
                    , viewEmptyState
                    ]

            else
                div [ class "pa4" ]
                    [ viewCommitHeader data.sha (Just data.jobs) (Just data.jobDetails)
                    , div [ class "mt4" ]
                        [ h2 [ class "f4 fw6 mb3" ]
                            [ text "Jobs" ]
                        , viewJobCards data
                        ]
                    ]


{-| View commit header with progress.
-}
viewCommitHeader : String -> Maybe (List CommitJob) -> Maybe (List ( Int, Maybe JobSetDetails )) -> Html Msg
viewCommitHeader sha maybeJobs maybeDetails =
    div [ class "mb4" ]
        [ h1 [ class "f2 fw6 mb2" ]
            [ text "Commit "
            , code [ class "monospace f4" ]
                [ text (String.left 8 sha) ]
            ]
        , case ( maybeJobs, maybeDetails ) of
            ( Just jobs, Just details ) ->
                viewOverallProgress jobs details

            _ ->
                text ""
        ]


{-| View overall progress across all jobs.
-}
viewOverallProgress : List CommitJob -> List ( Int, Maybe JobSetDetails ) -> Html Msg
viewOverallProgress jobs details =
    let
        totalBuilds =
            List.filterMap
                (\( _, maybeJobDetails ) ->
                    case maybeJobDetails of
                        Just d ->
                            Just d.totalDrvs

                        Nothing ->
                            Nothing
                )
                details
                |> List.sum

        completedBuilds =
            List.filterMap
                (\( _, maybeJobDetails ) ->
                    case maybeJobDetails of
                        Just d ->
                            Just d.completedDrvs

                        Nothing ->
                            Nothing
                )
                details
                |> List.sum
    in
    if totalBuilds > 0 then
        div [ class "f6 gray" ]
            [ text (String.fromInt completedBuilds ++ " / " ++ String.fromInt totalBuilds ++ " builds complete") ]

    else
        text ""


{-| View empty state when no jobs are found.
-}
viewEmptyState : Html Msg
viewEmptyState =
    div [ class "card pa4 tc" ]
        [ h2 [ class "f4 fw6 mb2" ]
            [ text "No Jobs" ]
        , p [ class "gray" ]
            [ text "No jobs found for this commit." ]
        ]


{-| View job cards (expandable).
-}
viewJobCards : LoadedData -> Html Msg
viewJobCards data =
    div []
        (List.map (viewJobCard data) data.jobs)


{-| View a single job card.
-}
viewJobCard : LoadedData -> CommitJob -> Html Msg
viewJobCard data job =
    let
        isExpanded =
            Set.member job.jobsetId data.expandedJobs

        jobDetails =
            findJobDetails job.jobsetId data.jobDetails

        expandIcon =
            if isExpanded then
                "▼"

            else
                "▶"
    in
    div [ class "card mb3" ]
        [ -- Job header (always visible, clickable to expand)
          button
            [ class "w-100 pa3 bn bg-transparent pointer tl hover-bg-light-gray"
            , onClick (ToggleJobExpanded job.jobsetId)
            ]
            [ div [ class "flex items-center justify-between" ]
                [ div [ class "flex items-center" ]
                    [ span [ class "f6 mr2" ] [ text expandIcon ]
                    , h3 [ class "f5 fw6 ma0" ] [ text job.jobName ]
                    , span [ class "ml3 gray f6" ]
                        [ text (job.owner ++ "/" ++ job.repoName) ]
                    ]
                , span [ class "link blue hover-dark-blue f6" ]
                    [ a
                        [ href (Route.toHref (Route.Job job.jobsetId))
                        , class "link blue hover-dark-blue"
                        ]
                        [ text "View Details →" ]
                    ]
                ]
            ]

        -- Expanded section (only visible when expanded)
        , if isExpanded then
            viewJobExpandedSection jobDetails

          else
            text ""
        ]


{-| Find job details by jobset ID.
-}
findJobDetails : Int -> List ( Int, Maybe JobSetDetails ) -> Maybe JobSetDetails
findJobDetails jobsetId details =
    details
        |> List.filter (\( id, _ ) -> id == jobsetId)
        |> List.head
        |> Maybe.andThen Tuple.second


{-| View the expanded section of a job card.
-}
viewJobExpandedSection : Maybe JobSetDetails -> Html Msg
viewJobExpandedSection maybeDetails =
    div [ class "pa3 bt b--black-10 bg-near-white" ]
        [ case maybeDetails of
            Just details ->
                viewJobDetails details

            Nothing ->
                Loader.viewWithMessage "Loading job details..."
        ]


{-| View job details with progress bar.
-}
viewJobDetails : JobSetDetails -> Html Msg
viewJobDetails details =
    let
        segments =
            ProgressBar.fromJobStats
                { completed = details.completedDrvs
                , failed = details.failedDrvs + details.transitiveFailureDrvs
                , building = details.buildingDrvs
                , queued = details.queuedDrvs + details.buildableDrvs
                }
    in
    div []
        [ -- Progress bar
          div [ class "mb3" ]
            [ ProgressBar.view details.totalDrvs segments ]

        -- Statistics
        , div [ class "flex justify-around f7 gray" ]
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

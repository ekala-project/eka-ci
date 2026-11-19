module Pages.Commit exposing
    ( Model
    , Msg(..)
    , init
    , update
    , view
    )

{-| Commit page: every job evaluated for a commit, with build progress.

Progress for all jobs loads up front (one request per job) and refreshes
when the server reports a job as complete.

-}

import Api.Api as Api
import Components.BuildProgress as Progress
import Components.Ui as Ui
import Dict exposing (Dict)
import Html exposing (Html, a, div, h1, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href, scope, title)
import Http
import Models.Job exposing (CommitJob, JobSetDetails)
import Ports
import Route


type Jobs
    = Loading
    | Ready (List CommitJob)
    | Failed Http.Error


type alias Model =
    { apiBaseUrl : String
    , sha : String
    , jobs : Jobs

    -- A missing entry means the job's details are still loading.
    , details : Dict Int (Result Http.Error JobSetDetails)
    }


type Msg
    = GotCommitJobs (Result Http.Error (List CommitJob))
    | GotJobDetails Int (Result Http.Error JobSetDetails)
    | JobCompleted Ports.JobCompleteEvent


init : String -> String -> ( Model, Cmd Msg )
init apiBaseUrl sha =
    ( { apiBaseUrl = apiBaseUrl, sha = sha, jobs = Loading, details = Dict.empty }
    , Cmd.batch
        [ Api.getCommitJobs apiBaseUrl sha GotCommitJobs
        , Ports.websocketOut (Ports.encodeSubscribeMessage "commit" sha)
        ]
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotCommitJobs (Ok jobs) ->
            ( { model | jobs = Ready jobs }
            , Cmd.batch (List.map (fetchDetails model.apiBaseUrl << .jobsetId) jobs)
            )

        GotCommitJobs (Err error) ->
            ( { model | jobs = Failed error }, Cmd.none )

        GotJobDetails jobsetId result ->
            ( { model | details = Dict.insert jobsetId result model.details }, Cmd.none )

        JobCompleted event ->
            if List.member event.jobsetId (jobIds model) then
                ( model, fetchDetails model.apiBaseUrl event.jobsetId )

            else
                ( model, Cmd.none )


fetchDetails : String -> Int -> Cmd Msg
fetchDetails apiBaseUrl jobsetId =
    Api.getJobSetDetails apiBaseUrl jobsetId (GotJobDetails jobsetId)


jobIds : Model -> List Int
jobIds model =
    case model.jobs of
        Ready jobs ->
            List.map .jobsetId jobs

        _ ->
            []



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ viewHead model
        , case model.jobs of
            Loading ->
                Ui.loading "Loading jobs…"

            Failed error ->
                Ui.httpError "Could not load jobs for this commit." error

            Ready [] ->
                Ui.emptyState "No jobs for this commit"
                    "EkaCI has not evaluated any jobs at this commit yet."

            Ready jobs ->
                div []
                    [ overallPanel model jobs
                    , Ui.panel "Jobs" [] [ Ui.tableWrap (jobTable model jobs) ]
                    ]
        ]


viewHead : Model -> Html Msg
viewHead model =
    let
        repoCrumb =
            case model.jobs of
                Ready (job :: _) ->
                    [ ( job.owner ++ "/" ++ job.repoName, Route.toHref (Route.Repository job.owner job.repoName) ) ]

                _ ->
                    []
    in
    Ui.pageHead
        { crumbs = ( "Repositories", Route.toHref Route.Home ) :: repoCrumb
        , title = [ h1 [] [ text "Commit ", span [ class "mono" ] [ text (Ui.shortSha model.sha) ] ] ]
        , lede = [ span [ class "path" ] [ text model.sha ] ]
        , actions = []
        }


{-| Combined progress across every job whose details have loaded.
-}
overallPanel : Model -> List CommitJob -> Html Msg
overallPanel model jobs =
    let
        loaded =
            List.filterMap (\job -> Dict.get job.jobsetId model.details |> Maybe.andThen Result.toMaybe) jobs

        counts =
            List.foldl (Progress.add << Progress.fromJobSet) Progress.empty loaded
    in
    if List.length jobs < 2 || List.isEmpty loaded then
        text ""

    else
        Ui.panel "All jobs" [ Progress.overallBadge counts ] [ Progress.view counts ]


jobTable : Model -> List CommitJob -> Html Msg
jobTable model jobs =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Job" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col" ] [ text "Progress" ]
                ]
            ]
        , tbody [] (List.map (jobRow model) jobs)
        ]


jobRow : Model -> CommitJob -> Html Msg
jobRow model job =
    let
        link =
            a [ class "cell-main", href (Route.toHref (Route.Job job.jobsetId)), title ("Job #" ++ String.fromInt job.jobsetId) ]
                [ text job.jobName ]
    in
    case Dict.get job.jobsetId model.details of
        Nothing ->
            tr []
                [ td [] [ link ]
                , td [] [ Ui.loadingInline "Loading" ]
                , td [ class "progress-cell" ] []
                ]

        Just (Err _) ->
            tr []
                [ td [] [ link ]
                , td [] [ Ui.badge Ui.Neutral "Unknown" ]
                , td [ class "progress-cell small muted" ] [ text "Could not load progress." ]
                ]

        Just (Ok details) ->
            let
                counts =
                    Progress.fromJobSet details
            in
            tr []
                [ td [] [ link ]
                , td [] [ Progress.overallBadge counts ]
                , td [ class "progress-cell" ] [ Progress.cell counts ]
                ]

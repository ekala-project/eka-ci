module Pages.Commit exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Commit page displaying jobs for a commit.

Shows all jobs (builds) associated with a specific commit SHA.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Html exposing (Html, a, code, div, h1, h2, p, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href)
import Http
import Models.Job exposing (CommitJob)
import Route


{-| Page model.
-}
type Model
    = Loading String
    | Loaded String (List CommitJob)
    | Failed Http.Error


{-| Page messages.
-}
type Msg
    = GotCommitJobs (Result Http.Error (List CommitJob))


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
                            ( Loaded sha jobs, Cmd.none )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading sha ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Commit "
                    , code [ class "monospace f4" ]
                        [ text (String.left 8 sha) ]
                    ]
                , Loader.viewWithMessage "Loading jobs..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Commit" ]
                , ErrorView.viewHttp error
                ]

        Loaded sha [] ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Commit "
                    , code [ class "monospace f4" ]
                        [ text (String.left 8 sha) ]
                    ]
                , viewEmptyState
                ]

        Loaded sha jobs ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Commit "
                    , code [ class "monospace f4" ]
                        [ text (String.left 8 sha) ]
                    ]
                , viewJobList jobs
                ]


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


{-| View the list of jobs.
-}
viewJobList : List CommitJob -> Html Msg
viewJobList jobs =
    div [ class "card" ]
        [ table [ class "table w-100" ]
            [ thead []
                [ tr []
                    [ th [] [ text "Job Name" ]
                    , th [] [ text "Repository" ]
                    , th [] [ text "Actions" ]
                    ]
                ]
            , tbody []
                (List.map viewJobRow jobs)
            ]
        ]


{-| View a single job row.
-}
viewJobRow : CommitJob -> Html Msg
viewJobRow job =
    tr []
        [ td []
            [ text job.jobName ]
        , td []
            [ a
                [ href (Route.toHref (Route.Repository job.owner job.repoName))
                , class "link blue hover-dark-blue"
                ]
                [ text (job.owner ++ "/" ++ job.repoName) ]
            ]
        , td []
            [ a
                [ href (Route.toHref (Route.Job job.jobsetId))
                , class "link blue hover-dark-blue"
                ]
                [ text "View Details" ]
            ]
        ]

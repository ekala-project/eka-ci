module Pages.Home exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Home page displaying list of repositories.

Shows all repositories where the GitHub App is installed.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Html exposing (Html, a, div, h1, h2, p, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href)
import Http
import Models.Repository exposing (Repository)
import Route


{-| Page model.
-}
type Model
    = Loading
    | Loaded (List Repository)
    | Failed Http.Error


{-| Page messages.
-}
type Msg
    = GotRepositories (Result Http.Error (List Repository))


{-| Initialize the page.
-}
init : ( Model, Cmd Msg )
init =
    ( Loading
    , Api.getRepositories GotRepositories
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotRepositories result ->
            case result of
                Ok repos ->
                    ( Loaded repos, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div [ class "pa4" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Repositories" ]
        , case model of
            Loading ->
                Loader.viewWithMessage "Loading repositories..."

            Failed error ->
                ErrorView.viewHttp error

            Loaded [] ->
                viewEmptyState

            Loaded repos ->
                viewRepositoryList repos
        ]


{-| View empty state when no repositories are found.
-}
viewEmptyState : Html Msg
viewEmptyState =
    div [ class "card pa4 tc" ]
        [ h2 [ class "f4 fw6 mb2" ]
            [ text "No Repositories" ]
        , p [ class "gray" ]
            [ text "No repositories found. Install the GitHub App on your repositories to get started." ]
        ]


{-| View the list of repositories.
-}
viewRepositoryList : List Repository -> Html Msg
viewRepositoryList repos =
    div [ class "card" ]
        [ table [ class "table w-100" ]
            [ thead []
                [ tr []
                    [ th [] [ text "Repository" ]
                    , th [] [ text "Owner" ]
                    , th [] [ text "Installation ID" ]
                    ]
                ]
            , tbody []
                (List.map viewRepositoryRow repos)
            ]
        ]


{-| View a single repository row.
-}
viewRepositoryRow : Repository -> Html Msg
viewRepositoryRow repo =
    tr []
        [ td []
            [ a
                [ href (Route.toHref (Route.Repository repo.owner repo.repoName))
                , class "link blue hover-dark-blue"
                ]
                [ text repo.repoName ]
            ]
        , td []
            [ text repo.owner ]
        , td []
            [ text (String.fromInt repo.installationId) ]
        ]

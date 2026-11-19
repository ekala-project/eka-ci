module Pages.Home exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Home page displaying list of repositories.

Shows all repositories where the GitHub App is installed as a card grid
with a name filter.

-}

import Api.Api as Api
import Components.Ui as Ui
import Html exposing (Html, a, div, input, label, li, p, span, text, ul)
import Html.Attributes exposing (attribute, class, href, id, placeholder, type_, value)
import Html.Events exposing (onInput)
import Http
import Models.Repository exposing (Repository)
import Route


{-| Page model with search functionality.
-}
type Model
    = Loading String
    | Loaded LoadedData
    | Failed Http.Error


{-| Loaded data with repositories and search term.
-}
type alias LoadedData =
    { repositories : List Repository
    , searchTerm : String
    , apiBaseUrl : String
    }


{-| Page messages.
-}
type Msg
    = GotRepositories (Result Http.Error (List Repository))
    | SearchInput String


{-| Initialize the page.
-}
init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( Loading apiBaseUrl
    , Api.getRepositories apiBaseUrl GotRepositories
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotRepositories result ->
            case ( result, model ) of
                ( Ok repos, Loading apiBaseUrl ) ->
                    ( Loaded
                        { repositories = repos
                        , searchTerm = ""
                        , apiBaseUrl = apiBaseUrl
                        }
                    , Cmd.none
                    )

                ( Ok repos, _ ) ->
                    -- Shouldn't happen, but handle gracefully
                    ( Loaded
                        { repositories = repos
                        , searchTerm = ""
                        , apiBaseUrl = ""
                        }
                    , Cmd.none
                    )

                ( Err error, _ ) ->
                    ( Failed error, Cmd.none )

        SearchInput term ->
            case model of
                Loaded data ->
                    ( Loaded { data | searchTerm = term }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div []
        [ Ui.simpleHead "Repositories" "Repositories where the EkaCI GitHub App is installed."
        , case model of
            Loading _ ->
                Ui.loading "Loading repositories…"

            Failed error ->
                Ui.httpError "Could not load repositories." error

            Loaded data ->
                if List.isEmpty data.repositories then
                    Ui.emptyState "No repositories yet"
                        "Install the EkaCI GitHub App on a repository and it will appear here."

                else
                    viewRepositories data
        ]


viewRepositories : LoadedData -> Html Msg
viewRepositories data =
    let
        filtered =
            filterRepositories data.searchTerm data.repositories

        total =
            List.length data.repositories
    in
    div []
        [ div [ class "toolbar" ]
            [ label [ class "field grow" ]
                [ text "Filter repositories"
                , input
                    [ type_ "search"
                    , placeholder "owner or name"
                    , value data.searchTerm
                    , onInput SearchInput
                    , attribute "aria-describedby" "repo-count"
                    ]
                    []
                ]
            ]
        , p [ id "repo-count", class "small muted", attribute "aria-live" "polite" ]
            [ text (countText (List.length filtered) total) ]
        , if List.isEmpty filtered then
            Ui.emptyState "No matching repositories" ("Nothing matches “" ++ data.searchTerm ++ "”.")

          else
            ul [ class "repo-grid" ] (List.map viewRepositoryCard filtered)
        ]


countText : Int -> Int -> String
countText shown total =
    if shown == total then
        String.fromInt total ++ pluralize total " repository" " repositories"

    else
        String.fromInt shown ++ " of " ++ String.fromInt total ++ " repositories"


pluralize : Int -> String -> String -> String
pluralize n one many =
    if n == 1 then
        one

    else
        many


filterRepositories : String -> List Repository -> List Repository
filterRepositories term repos =
    let
        needle =
            String.toLower (String.trim term)
    in
    if String.isEmpty needle then
        repos

    else
        List.filter
            (\repo -> String.contains needle (String.toLower (repo.owner ++ "/" ++ repo.repoName)))
            repos


viewRepositoryCard : Repository -> Html Msg
viewRepositoryCard repo =
    li []
        [ a [ class "repo-card", href (Route.toHref (Route.Repository repo.owner repo.repoName)) ]
            [ span [ class "repo-owner" ] [ text repo.owner ]
            , span [ class "repo-name" ] [ text repo.repoName ]
            ]
        ]

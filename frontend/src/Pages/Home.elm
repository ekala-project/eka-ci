module Pages.Home exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Home page displaying list of repositories.

Shows all repositories where the GitHub App is installed with a
Buildkite-style card layout, search, and filtering.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Html exposing (Html, a, div, h1, h2, input, p, span, text)
import Html.Attributes exposing (class, href, placeholder, type_, value)
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
    div [ class "pa4" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Repositories" ]
        , case model of
            Loading _ ->
                Loader.viewWithMessage "Loading repositories..."

            Failed error ->
                ErrorView.viewHttp error

            Loaded data ->
                if List.isEmpty data.repositories then
                    viewEmptyState

                else
                    viewRepositoryGrid data
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


{-| View the repository grid with search.
-}
viewRepositoryGrid : LoadedData -> Html Msg
viewRepositoryGrid data =
    let
        filteredRepos =
            filterRepositories data.searchTerm data.repositories
    in
    div []
        [ -- Search bar
          viewSearchBar data.searchTerm

        -- Repository count
        , div [ class "mv3 gray f6" ]
            [ text
                (String.fromInt (List.length filteredRepos)
                    ++ " "
                    ++ (if List.length filteredRepos == 1 then
                            "repository"

                        else
                            "repositories"
                       )
                )
            ]

        -- Repository cards
        , if List.isEmpty filteredRepos then
            div [ class "card pa4 tc gray" ]
                [ text "No repositories match your search." ]

          else
            div [ class "flex flex-wrap nl3 nr3" ]
                (List.map viewRepositoryCard filteredRepos)
        ]


{-| View search bar.
-}
viewSearchBar : String -> Html Msg
viewSearchBar searchTerm =
    div [ class "mb3" ]
        [ input
            [ type_ "text"
            , class "pa3 br2 ba b--black-20 w-100 w-50-ns"
            , placeholder "Search repositories by name or owner..."
            , value searchTerm
            , onInput SearchInput
            ]
            []
        ]


{-| Filter repositories by search term.
-}
filterRepositories : String -> List Repository -> List Repository
filterRepositories searchTerm repos =
    if String.isEmpty searchTerm then
        repos

    else
        let
            lowerTerm =
                String.toLower searchTerm
        in
        List.filter
            (\repo ->
                String.contains lowerTerm (String.toLower repo.repoName)
                    || String.contains lowerTerm (String.toLower repo.owner)
            )
            repos


{-| View a single repository card.
-}
viewRepositoryCard : Repository -> Html Msg
viewRepositoryCard repo =
    div [ class "w-100 w-50-m w-third-l pa3" ]
        [ a
            [ href (Route.toHref (Route.Repository repo.owner repo.repoName))
            , class "card db pa4 link black hover-bg-light-gray transition"
            ]
            [ -- Repository name
              h2 [ class "f4 fw6 mt0 mb2 dark-blue" ]
                [ text repo.repoName ]

            -- Owner
            , div [ class "flex items-center mb2" ]
                [ span [ class "gray f6" ]
                    [ text ("@" ++ repo.owner) ]
                ]

            -- Installation ID (subtle)
            , div [ class "mt3 pt3 bt b--black-10" ]
                [ span [ class "f7 gray" ]
                    [ text ("Installation #" ++ String.fromInt repo.installationId) ]
                ]
            ]
        ]

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
import Html exposing (Html, a, div, h1, h2, p, text)
import Html.Attributes exposing (class, href)
import Http
import Models.Repository exposing (Repository)


{-| Page model.
-}
type Model
    = Loading String String String
    | Loaded Repository
    | Failed Http.Error


{-| Page messages.
-}
type Msg
    = GotRepository (Result Http.Error Repository)


{-| Initialize the page with owner and repository name.
-}
init : String -> String -> String -> ( Model, Cmd Msg )
init apiBaseUrl owner repoName =
    ( Loading apiBaseUrl owner repoName
    , Api.getRepository apiBaseUrl owner repoName GotRepository
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotRepository result ->
            case result of
                Ok repo ->
                    ( Loaded repo, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )


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

        Loaded repo ->
            viewRepositoryDetails repo


{-| View repository details.
-}
viewRepositoryDetails : Repository -> Html Msg
viewRepositoryDetails repo =
    div [ class "pa4" ]
        [ -- Header
          h1 [ class "f2 fw6 mb2" ]
            [ text (repo.owner ++ "/" ++ repo.repoName) ]

        -- Metadata
        , div [ class "gray f6 mb4" ]
            [ text ("Installation ID: " ++ String.fromInt repo.installationId) ]

        -- Repository info card
        , div [ class "card pa4 mb4" ]
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Repository Information" ]
            , viewInfoRow "Owner" repo.owner
            , viewInfoRow "Repository" repo.repoName
            , viewInfoRow "Installation ID" (String.fromInt repo.installationId)
            ]

        -- Placeholder for recent builds
        , div [ class "card pa4" ]
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Recent Builds" ]
            , p [ class "gray" ]
                [ text "Recent builds functionality coming soon..." ]
            , p [ class "gray f6 mt2" ]
                [ text "Future features:" ]
            , div [ class "ml3 mt2 gray f6" ]
                [ p [] [ text "• List of recent commits" ]
                , p [] [ text "• Build status for each commit" ]
                , p [] [ text "• Filter by branch" ]
                , p [] [ text "• Search commits" ]
                ]
            ]
        ]


{-| View an information row.
-}
viewInfoRow : String -> String -> Html Msg
viewInfoRow label value =
    div [ class "flex pv2 bb b--black-10" ]
        [ div [ class "w-30 fw6" ] [ text label ]
        , div [ class "w-70" ] [ text value ]
        ]

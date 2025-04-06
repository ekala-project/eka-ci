module Main exposing (main)

import Browser
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Http
import Json.Decode as De exposing (Decoder)
import Task
import Url exposing (Url)


main : Program () Model Msg
main =
    Browser.application
        { init = init
        , view = view
        , update = update
        , subscriptions = subscriptions
        , onUrlChange = UrlChanged
        , onUrlRequest = LinkClicked
        }



-- MODEL
-- The urls are only needed as href/src so we do not need to parse/store them as an actual Url type.


type alias Repository =
    { owner : String
    , name : String
    , description : String
    , url : String
    , avatar_url : String
    }


repositoryDecoder : Decoder Repository
repositoryDecoder =
    De.map5 Repository
        (De.field "owner" De.string)
        (De.field "name" De.string)
        (De.field "description" De.string)
        (De.field "url" De.string)
        (De.field "avatar_url" De.string)


type RepoStatus
    = Loading
    | Success (List Repository)
    | Failure


type alias Model =
    { navKey : Nav.Key
    , currentUrl : Url.Url
    , repos : RepoStatus
    }


init : () -> Url.Url -> Nav.Key -> ( Model, Cmd Msg )
init flags url key =
    ( Model key url Loading
    , getRepos
    )



-- UPDATE


type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url.Url
    | GotRepos (Result Http.Error (List Repository))


getRepos : Cmd Msg
getRepos =
    Http.get
        { url = "/api/repos"
        , expect = Http.expectJson GotRepos (De.list repositoryDecoder)
        }



-- Task.perform GotRepos
--     (Task.succeed
--         (Result.Ok
--             [ { owner = "ekala-project", name = "eka-ci", description = "CI/CD tool and web frontend for nix package sets", url = "https://github.com/ekala-project/eka-ci", avatar_url = "https://avatars.githubusercontent.com/u/172489582?v=4" }
--             , { owner = "rust-lang", name = "rust", description = "Empowering everyone to build reliable and efficient software.", url = "https://github.com/rust-lang/rust", avatar_url = "https://avatars.githubusercontent.com/u/5430905?v=4" }
--             , { owner = "elm", name = "compiler", description = "Compiler for Elm, a functional language for reliable webapps.", url = "https://github.com/elm/compiler", avatar_url = "https://avatars.githubusercontent.com/u/20698192?v=4" }
--             , { owner = "NixOS", name = "nixpkgs", description = "Nix Packages collection & NixOS", url = "https://github.com/NixOS/nixpkgs", avatar_url = "https://avatars.githubusercontent.com/u/487568?v=4" }
--             ]
--         )
--     )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        LinkClicked urlRequest ->
            case urlRequest of
                Browser.Internal url ->
                    ( model, Nav.pushUrl model.navKey (Url.toString url) )

                Browser.External href ->
                    ( model, Nav.load href )

        UrlChanged url ->
            ( { model | currentUrl = url }
            , Cmd.none
            )

        GotRepos result ->
            case result of
                -- Ok repos ->
                --     ( { model | repos = Success repos }, Cmd.none )
                Ok repos ->
                    ( { model | repos = Success repos }, Cmd.none )

                Err _ ->
                    ( { model | repos = Failure }, Cmd.none )



-- SUBSCRIPTIONS


subscriptions : Model -> Sub Msg
subscriptions _ =
    Sub.none



-- VIEW


view : Model -> Browser.Document Msg
view model =
    { title = "Project Ekala CI"
    , body =
        [ div
            [ style "display" "grid"
            , style "width" "100%"
            , style "height" "max(100vh, 100%)"
            , style "grid-template-rows" "60px 1fr 60px"
            , style "grid-template-columns" "1fr"
            , style "grid-template-areas" "'header' 'main' 'footer'"
            ]
            [ div
                [ style "grid-area" "header"
                , style "display" "flex"
                , style "flex-direction" "row"
                , style "justify-content" "space-between"
                , style "align-items" "center"
                , style "padding" ".5rem 12%"
                , style "border-bottom" "1px solid var(--primary)"
                ]
                [ a
                    [ href "/"
                    , style "color" "inherit"
                    , style "text-decoration" "none"
                    ]
                    [ p
                        [ style "font-weight" "700" ]
                        [ text "Project Ekala CI" ]
                    ]
                ]
            , div
                [ style "grid-area" "main"
                , style "padding" "2rem 12%"
                ]
                [ h1 [ style "margin-top" "0", style "color" "var(--primary)" ] [ text "Connected Repositories" ]
                , viewRepos model
                ]
            ]
        ]
    }


viewRepos : Model -> Html Msg
viewRepos model =
    case model.repos of
        Failure ->
            div [] [ p [] [ text "Failed to load repository list for some reason." ] ]

        Loading ->
            p [] [ text "Loading..." ]

        Success repos ->
            ul
                [ style "display" "table"
                , style "width" "100%"
                , style "margin" "0 0.5%"
                , style "list-style" "none"
                ]
                (List.map viewRepo repos)


viewRepo : Repository -> Html Msg
viewRepo repo =
    li
        [ style "display" "grid"
        , style "width" "100%"
        , style "grid-template-rows" "max-content"
        , style "grid-template-columns" "auto 1fr"
        , style "column-gap" "20px"
        , style "grid-template-areas" "'avatar text'"
        , style "border-top" "1px solid var(--primary)"
        , style "padding" "0.75rem 0"
        , style "vertical-align" "top"
        ]
        [ div
            [ style "grid-area" "avatar"
            , style "padding-left" "4px"
            ]
            [ img
                [ src repo.avatar_url
                , style "width" "48px"
                , style "height" "48px"
                , style "border-radius" "12px"
                , style "border" "1px solid var(--secondary)"
                ]
                []
            ]
        , div
            [ style "grid-area" "text"
            , style "display" "flex"
            , style "flex-direction" "column"
            , style "gap" "0.4rem"
            , style "justify-content" "start"
            ]
            [ a
                [ href repo.url
                , style "width" "max-content"
                , style "color" "inherit"
                , style "text-decoration" "none"
                ]
                [ p [ style "line-height" "1.5"]
                    [ text (repo.owner ++ " / ")
                    , span [ style "font-weight" "700" ] [ text repo.name ]
                    ]
                ]
            , p
                [ style "font-size" "0.9rem"
                , style "color" "color-mix(in srgb, var(--text), white 40%)"
                ]
                [ text repo.description ]
            ]
        ]

module Main exposing (main)

{-| Main application entry point.

This module sets up routing, manages page state, and coordinates
the overall application structure.

-}

import Browser
import Browser.Navigation as Nav
import Components.Header as Header
import Html exposing (Html, div, text)
import Html.Attributes exposing (class)
import Json.Encode as E
import Pages.Admin as Admin
import Pages.Commit as Commit
import Pages.Home as Home
import Ports
import Route exposing (Route)
import Url exposing (Url)


{-| Main program entry point.
-}
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


{-| Application model tracking navigation and current page.
-}
type alias Model =
    { navKey : Nav.Key
    , route : Route
    , page : Page
    }


{-| The current page and its state.
-}
type Page
    = HomePage Home.Model
    | CommitPage Commit.Model
    | AdminPage Admin.Model
    | NotFoundPage


{-| Initialize the application.
-}
init : () -> Url -> Nav.Key -> ( Model, Cmd Msg )
init _ url navKey =
    let
        route =
            Route.fromUrl url

        ( page, cmd ) =
            initPage route
    in
    ( { navKey = navKey
      , route = route
      , page = page
      }
    , cmd
    )


{-| Initialize a page based on the current route.
-}
initPage : Route -> ( Page, Cmd Msg )
initPage route =
    case route of
        Route.Home ->
            let
                ( model, cmd ) =
                    Home.init
            in
            ( HomePage model, Cmd.map HomeMsg cmd )

        Route.Commit sha ->
            let
                ( model, cmd ) =
                    Commit.init sha
            in
            ( CommitPage model, Cmd.map CommitMsg cmd )

        Route.Repository _ _ ->
            -- TODO: Implement repository page
            ( NotFoundPage, Cmd.none )

        Route.Job _ ->
            -- TODO: Implement job page
            ( NotFoundPage, Cmd.none )

        Route.Drv _ ->
            -- TODO: Implement derivation page
            ( NotFoundPage, Cmd.none )

        Route.NotFound ->
            ( NotFoundPage, Cmd.none )



-- UPDATE


{-| Application messages.
-}
type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url
    | HomeMsg Home.Msg
    | CommitMsg Commit.Msg
    | AdminMsg Admin.Msg
    | WebSocketMessage Ports.IncomingMessage


{-| Update the application state.
-}
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
            let
                newRoute =
                    Route.fromUrl url

                ( newPage, cmd ) =
                    initPage newRoute
            in
            ( { model
                | route = newRoute
                , page = newPage
              }
            , cmd
            )

        HomeMsg homeMsg ->
            case model.page of
                HomePage homeModel ->
                    let
                        ( newModel, cmd ) =
                            Home.update homeMsg homeModel
                    in
                    ( { model | page = HomePage newModel }
                    , Cmd.map HomeMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        CommitMsg commitMsg ->
            case model.page of
                CommitPage commitModel ->
                    let
                        ( newModel, cmd ) =
                            Commit.update commitMsg commitModel
                    in
                    ( { model | page = CommitPage newModel }
                    , Cmd.map CommitMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        AdminMsg adminMsg ->
            case model.page of
                AdminPage adminModel ->
                    let
                        ( newModel, cmd ) =
                            Admin.update adminMsg adminModel
                    in
                    ( { model | page = AdminPage newModel }
                    , Cmd.map AdminMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        WebSocketMessage wsMsg ->
            -- TODO: Handle WebSocket messages (build state changes, etc.)
            ( model, Cmd.none )



-- SUBSCRIPTIONS


{-| Application subscriptions.
-}
subscriptions : Model -> Sub Msg
subscriptions model =
    Ports.websocketIn handleWebSocketMessage


{-| Handle incoming WebSocket messages.
-}
handleWebSocketMessage : E.Value -> Msg
handleWebSocketMessage value =
    case Ports.decodeIncomingMessage value of
        Ok message ->
            WebSocketMessage message

        Err error ->
            -- Ignore decode errors for now
            -- TODO: Log decode errors
            WebSocketMessage (Ports.Error "Failed to decode message")



-- VIEW


{-| View the application.
-}
view : Model -> Browser.Document Msg
view model =
    { title = pageTitle model.route
    , body =
        [ div []
            [ Header.view
            , viewPage model.page
            ]
        ]
    }


{-| Get the page title based on the current route.
-}
pageTitle : Route -> String
pageTitle route =
    case route of
        Route.Home ->
            "Repositories - EkaCI"

        Route.Repository owner repo ->
            owner ++ "/" ++ repo ++ " - EkaCI"

        Route.Commit sha ->
            "Commit " ++ String.left 8 sha ++ " - EkaCI"

        Route.Job jobsetId ->
            "Job #" ++ String.fromInt jobsetId ++ " - EkaCI"

        Route.Drv drvPath ->
            "Derivation - EkaCI"

        Route.NotFound ->
            "Not Found - EkaCI"


{-| View the current page.
-}
viewPage : Page -> Html Msg
viewPage page =
    case page of
        HomePage model ->
            Html.map HomeMsg (Home.view model)

        CommitPage model ->
            Html.map CommitMsg (Commit.view model)

        AdminPage model ->
            Html.map AdminMsg (Admin.view model)

        NotFoundPage ->
            viewNotFound


{-| View the 404 not found page.
-}
viewNotFound : Html Msg
viewNotFound =
    div [ class "pa4" ]
        [ div [ class "card pa4 tc" ]
            [ Html.h1 [ class "f2 fw6 mb2" ]
                [ text "404 - Not Found" ]
            , Html.p [ class "gray" ]
                [ text "The page you're looking for doesn't exist." ]
            ]
        ]

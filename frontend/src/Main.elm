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
import Pages.Drv as Drv
import Pages.Home as Home
import Pages.Job as Job
import Pages.Repository as Repository
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
    | RepositoryPage Repository.Model
    | CommitPage Commit.Model
    | JobPage Job.Model
    | DrvPage Drv.Model
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

        Route.Repository owner repo ->
            let
                ( model, cmd ) =
                    Repository.init owner repo
            in
            ( RepositoryPage model, Cmd.map RepositoryMsg cmd )

        Route.Commit sha ->
            let
                ( model, cmd ) =
                    Commit.init sha
            in
            ( CommitPage model, Cmd.map CommitMsg cmd )

        Route.Job jobsetId ->
            let
                ( model, cmd ) =
                    Job.init jobsetId
            in
            ( JobPage model, Cmd.map JobMsg cmd )

        Route.Drv drvPath ->
            let
                ( model, cmd ) =
                    Drv.init drvPath
            in
            ( DrvPage model, Cmd.map DrvMsg cmd )

        Route.NotFound ->
            ( NotFoundPage, Cmd.none )



-- UPDATE


{-| Application messages.
-}
type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url
    | HomeMsg Home.Msg
    | RepositoryMsg Repository.Msg
    | CommitMsg Commit.Msg
    | JobMsg Job.Msg
    | DrvMsg Drv.Msg
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

        RepositoryMsg repoMsg ->
            case model.page of
                RepositoryPage repoModel ->
                    let
                        ( newModel, cmd ) =
                            Repository.update repoMsg repoModel
                    in
                    ( { model | page = RepositoryPage newModel }
                    , Cmd.map RepositoryMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        JobMsg jobMsg ->
            case model.page of
                JobPage jobModel ->
                    let
                        ( newModel, cmd ) =
                            Job.update jobMsg jobModel
                    in
                    ( { model | page = JobPage newModel }
                    , Cmd.map JobMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        DrvMsg drvMsg ->
            case model.page of
                DrvPage drvModel ->
                    let
                        ( newModel, cmd ) =
                            Drv.update drvMsg drvModel
                    in
                    ( { model | page = DrvPage newModel }
                    , Cmd.map DrvMsg cmd
                    )

                _ ->
                    ( model, Cmd.none )

        WebSocketMessage wsMsg ->
            case wsMsg of
                Ports.Connected ->
                    -- WebSocket connected, no action needed
                    ( model, Cmd.none )

                Ports.Disconnected ->
                    -- WebSocket disconnected, no action needed
                    -- TODO: Show connection status indicator
                    ( model, Cmd.none )

                Ports.Error errorMsg ->
                    -- WebSocket error, no action needed
                    -- TODO: Log error or show to user
                    ( model, Cmd.none )

                Ports.BuildStateChange event ->
                    -- Route to Job or Drv pages
                    case model.page of
                        JobPage jobModel ->
                            -- TODO: Add BuildStateChanged message to Job.elm
                            ( model, Cmd.none )

                        DrvPage drvModel ->
                            -- TODO: Add BuildStateChanged message to Drv.elm
                            ( model, Cmd.none )

                        _ ->
                            ( model, Cmd.none )

                Ports.JobComplete event ->
                    -- Route to Job or Commit pages
                    case model.page of
                        JobPage jobModel ->
                            -- TODO: Add JobCompleted message to Job.elm
                            ( model, Cmd.none )

                        CommitPage commitModel ->
                            -- TODO: Add JobCompleted message to Commit.elm
                            ( model, Cmd.none )

                        _ ->
                            ( model, Cmd.none )

                Ports.LogLine event ->
                    -- Route to Drv page for log viewer
                    case model.page of
                        DrvPage drvModel ->
                            -- TODO: Add LogLineReceived message to Drv.elm
                            ( model, Cmd.none )

                        _ ->
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

        RepositoryPage model ->
            Html.map RepositoryMsg (Repository.view model)

        CommitPage model ->
            Html.map CommitMsg (Commit.view model)

        JobPage model ->
            Html.map JobMsg (Job.view model)

        DrvPage model ->
            Html.map DrvMsg (Drv.view model)

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

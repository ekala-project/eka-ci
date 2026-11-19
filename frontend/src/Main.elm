module Main exposing (main)

{-| Application entry point: routing, page state, auth, theme and the
shared WebSocket status.
-}

import Auth exposing (AuthState, User)
import Browser
import Browser.Dom as Dom
import Browser.Navigation as Nav
import Components.Header as Header
import Components.Ui as Ui
import Html exposing (Html, a, main_, text)
import Html.Attributes exposing (class, href, id, tabindex)
import Http
import Json.Encode as E
import Pages.Admin as Admin
import Pages.AttrPathSearch as AttrPathSearch
import Pages.AuthCallback as AuthCallback
import Pages.Builds as Builds
import Pages.Commit as Commit
import Pages.Drv as Drv
import Pages.Home as Home
import Pages.Job as Job
import Pages.Profile as Profile
import Pages.Repository as Repository
import Pages.Reviews as Reviews
import Ports
import Route exposing (Route)
import Task
import Theme exposing (Theme)
import Url exposing (Url)


type alias Flags =
    { apiBaseUrl : String
    , theme : String
    }


main : Program Flags Model Msg
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


type alias Model =
    { navKey : Nav.Key
    , url : Url
    , route : Route
    , page : Page
    , authState : AuthState
    , apiBaseUrl : String
    , theme : Theme
    , live : Header.Live
    }


type Page
    = HomePage Home.Model
    | BuildsPage Builds.Model
    | ReviewsPage Reviews.Model
    | RepositoryPage Repository.Model
    | CommitPage Commit.Model
    | JobPage Job.Model
    | DrvPage Drv.Model
    | AdminPage Admin.Model
    | ProfilePage Profile.Model
    | AttrPathSearchPage AttrPathSearch.Model
    | AuthCallbackPage AuthCallback.Model
      -- A signed-in page, waiting for (or lacking) a signed-in user.
    | AuthRequiredPage
    | ForbiddenPage
    | NotFoundPage


init : Flags -> Url -> Nav.Key -> ( Model, Cmd Msg )
init flags url navKey =
    let
        route =
            Route.fromUrl url

        ( page, cmd ) =
            initPage flags.apiBaseUrl url route Auth.init
    in
    ( { navKey = navKey
      , url = url
      , route = route
      , page = page
      , authState = Auth.init
      , apiBaseUrl = flags.apiBaseUrl
      , theme = Theme.fromString flags.theme
      , live = Header.Connecting
      }
    , cmd
    )


initPage : String -> Url -> Route -> AuthState -> ( Page, Cmd Msg )
initPage apiBaseUrl url route authState =
    case route of
        Route.Home ->
            Home.init apiBaseUrl |> initWith HomePage HomeMsg

        Route.Builds ->
            Builds.init apiBaseUrl
                |> initWith BuildsPage BuildsMsg
                |> Tuple.mapSecond (\cmd -> Cmd.batch [ cmd, Ports.websocketOut (Ports.encodeSubscribeMessage "allbuilds" "all") ])

        Route.Reviews ->
            Reviews.init apiBaseUrl |> initWith ReviewsPage ReviewsMsg

        Route.PullRequest _ _ _ ->
            ( NotFoundPage, Cmd.none )

        Route.Repository owner repo ->
            Repository.init apiBaseUrl owner repo |> initWith RepositoryPage RepositoryMsg

        Route.Commit sha ->
            Commit.init apiBaseUrl sha |> initWith CommitPage CommitMsg

        Route.Job jobsetId ->
            Job.init apiBaseUrl jobsetId authState |> initWith JobPage JobMsg

        Route.Drv drvPath ->
            Drv.init apiBaseUrl drvPath |> initWith DrvPage DrvMsg

        Route.Admin ->
            signedIn authState True (\token -> Admin.init apiBaseUrl token |> initWith AdminPage AdminMsg)

        Route.Profile ->
            signedIn authState False (\token -> Profile.init apiBaseUrl token |> initWith ProfilePage ProfileMsg)

        Route.AttrPathSearch ->
            AttrPathSearch.init apiBaseUrl authState |> initWith AttrPathSearchPage AttrPathSearchMsg

        Route.AuthCallback ->
            AuthCallback.init url |> initWith AuthCallbackPage AuthCallbackMsg

        Route.NotFound ->
            ( NotFoundPage, Cmd.none )


initWith : (subModel -> Page) -> (subMsg -> Msg) -> ( subModel, Cmd subMsg ) -> ( Page, Cmd Msg )
initWith toPage toMsg ( subModel, cmd ) =
    ( toPage subModel, Cmd.map toMsg cmd )


{-| Pages that need a signed-in user (and maybe an admin) wait until the user
is known, so they never fire requests that are bound to fail.
-}
signedIn : AuthState -> Bool -> (String -> ( Page, Cmd Msg )) -> ( Page, Cmd Msg )
signedIn authState adminOnly build =
    case ( Auth.getToken authState, authState.user ) of
        ( Just token, Just user ) ->
            if adminOnly && not user.isAdmin then
                ( ForbiddenPage, Cmd.none )

            else
                build token

        _ ->
            ( AuthRequiredPage, Cmd.none )



-- UPDATE


type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url
    | HomeMsg Home.Msg
    | BuildsMsg Builds.Msg
    | ReviewsMsg Reviews.Msg
    | RepositoryMsg Repository.Msg
    | CommitMsg Commit.Msg
    | JobMsg Job.Msg
    | DrvMsg Drv.Msg
    | AdminMsg Admin.Msg
    | ProfileMsg Profile.Msg
    | AttrPathSearchMsg AttrPathSearch.Msg
    | AuthCallbackMsg AuthCallback.Msg
    | WebSocketMessage Ports.IncomingMessage
    | TokenReceived (Maybe String)
    | GotUserInfo String (Result Http.Error User)
    | LogoutRequested
    | ThemeToggled
    | NoOp


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        LinkClicked request ->
            linkClicked request model

        UrlChanged url ->
            urlChanged url model

        WebSocketMessage incoming ->
            routeSocketMessage incoming model

        TokenReceived (Just token) ->
            ( { model | authState = Auth.updateToken (Just token) model.authState }, fetchUserInfo token )

        TokenReceived Nothing ->
            ( withAuth (Auth.finishLoading model.authState) model, Cmd.none )

        GotUserInfo token result ->
            gotUserInfo token result model

        LogoutRequested ->
            ( withAuth (Auth.logout model.authState) model
            , Cmd.batch [ Ports.clearToken (), Nav.pushUrl model.navKey (Route.toHref Route.Home) ]
            )

        ThemeToggled ->
            let
                theme =
                    Theme.next model.theme
            in
            ( { model | theme = theme }, Ports.setTheme (Theme.toString theme) )

        NoOp ->
            ( model, Cmd.none )

        _ ->
            updatePage msg model


linkClicked : Browser.UrlRequest -> Model -> ( Model, Cmd Msg )
linkClicked request model =
    case request of
        Browser.Internal url ->
            if isBackendRoute url then
                ( model, Nav.load (Url.toString url) )

            else if url.path == model.url.path && url.query == model.url.query && url.fragment /= Nothing then
                -- In-page anchor (e.g. the skip link): move focus, don't re-route.
                ( model, focus (Maybe.withDefault "" url.fragment) )

            else
                ( model, Nav.pushUrl model.navKey (Url.toString url) )

        Browser.External href ->
            ( model, Nav.load href )


urlChanged : Url -> Model -> ( Model, Cmd Msg )
urlChanged url model =
    let
        route =
            Route.fromUrl url

        ( page, cmd ) =
            initPage model.apiBaseUrl url route model.authState
    in
    ( { model | url = url, route = route, page = page }
    , Cmd.batch
        [ unsubscribeFromPage model.page
        , cmd
        , Task.perform (\_ -> NoOp) (Dom.setViewport 0 0)
        ]
    )


focus : String -> Cmd Msg
focus elementId =
    Task.attempt (\_ -> NoOp) (Dom.focus elementId)


gotUserInfo : String -> Result Http.Error User -> Model -> ( Model, Cmd Msg )
gotUserInfo token result model =
    if Auth.getToken model.authState /= Just token then
        -- A newer sign-in (or sign-out) superseded this lookup.
        ( model, Cmd.none )

    else
        case result of
            Ok user ->
                let
                    signedInModel =
                        withAuth (Auth.updateUser (Just user) model.authState) model
                in
                case model.page of
                    AuthRequiredPage ->
                        let
                            ( page, cmd ) =
                                initPage model.apiBaseUrl model.url model.route signedInModel.authState
                        in
                        ( { signedInModel | page = page }, cmd )

                    _ ->
                        ( signedInModel, Cmd.none )

            Err _ ->
                ( withAuth (Auth.logout model.authState) model, Ports.clearToken () )


{-| Update the auth state and hand it to pages that show auth-dependent UI.
-}
withAuth : AuthState -> Model -> Model
withAuth authState model =
    let
        page =
            case model.page of
                JobPage m ->
                    JobPage (Job.setAuth authState m)

                AttrPathSearchPage m ->
                    AttrPathSearchPage (AttrPathSearch.setAuth authState m)

                other ->
                    other
    in
    { model | authState = authState, page = page }


updatePage : Msg -> Model -> ( Model, Cmd Msg )
updatePage msg model =
    case ( msg, model.page ) of
        ( HomeMsg sub, HomePage m ) ->
            Home.update sub m |> updateWith HomePage HomeMsg model

        ( BuildsMsg sub, BuildsPage m ) ->
            Builds.update sub m |> updateWith BuildsPage BuildsMsg model

        ( ReviewsMsg sub, ReviewsPage m ) ->
            Reviews.update sub m |> updateWith ReviewsPage ReviewsMsg model

        ( RepositoryMsg sub, RepositoryPage m ) ->
            Repository.update sub m |> updateWith RepositoryPage RepositoryMsg model

        ( CommitMsg sub, CommitPage m ) ->
            Commit.update sub m |> updateWith CommitPage CommitMsg model

        ( JobMsg sub, JobPage m ) ->
            Job.update sub m |> updateWith JobPage JobMsg model

        ( DrvMsg sub, DrvPage m ) ->
            Drv.update sub m |> updateWith DrvPage DrvMsg model

        ( AdminMsg sub, AdminPage m ) ->
            Admin.update sub m |> updateWith AdminPage AdminMsg model

        ( ProfileMsg sub, ProfilePage m ) ->
            Profile.update sub m |> updateWith ProfilePage ProfileMsg model

        ( AttrPathSearchMsg sub, AttrPathSearchPage m ) ->
            AttrPathSearch.update sub m |> updateWith AttrPathSearchPage AttrPathSearchMsg model

        ( AuthCallbackMsg sub, AuthCallbackPage m ) ->
            AuthCallback.update sub m |> authCallbackUpdated model

        _ ->
            ( model, Cmd.none )


updateWith : (subModel -> Page) -> (subMsg -> Msg) -> Model -> ( subModel, Cmd subMsg ) -> ( Model, Cmd Msg )
updateWith toPage toMsg model ( subModel, cmd ) =
    ( { model | page = toPage subModel }, Cmd.map toMsg cmd )


{-| On a successful sign-in, store the token and go home.
-}
authCallbackUpdated : Model -> ( AuthCallback.Model, Cmd AuthCallback.Msg ) -> ( Model, Cmd Msg )
authCallbackUpdated model ( subModel, cmd ) =
    case subModel of
        AuthCallback.Success token user ->
            ( withAuth (model.authState |> Auth.updateToken (Just token) |> Auth.updateUser (Just user))
                { model | page = AuthCallbackPage subModel }
            , Cmd.batch
                [ Cmd.map AuthCallbackMsg cmd
                , Ports.storeToken token
                , Nav.replaceUrl model.navKey (Route.toHref Route.Home)
                ]
            )

        _ ->
            ( { model | page = AuthCallbackPage subModel }, Cmd.map AuthCallbackMsg cmd )


routeSocketMessage : Ports.IncomingMessage -> Model -> ( Model, Cmd Msg )
routeSocketMessage incoming model =
    case ( incoming, model.page ) of
        ( Ports.Connected, _ ) ->
            ( { model | live = Header.Connected }, Cmd.none )

        ( Ports.Disconnected, _ ) ->
            ( { model | live = Header.Offline }, Cmd.none )

        ( Ports.BuildStateChange event, JobPage m ) ->
            Job.update (Job.BuildStateChanged event) m |> updateWith JobPage JobMsg model

        ( Ports.BuildStateChange event, DrvPage m ) ->
            Drv.update (Drv.BuildStateChanged event) m |> updateWith DrvPage DrvMsg model

        ( Ports.JobComplete event, JobPage m ) ->
            Job.update (Job.JobCompleted event) m |> updateWith JobPage JobMsg model

        ( Ports.JobComplete event, CommitPage m ) ->
            Commit.update (Commit.JobCompleted event) m |> updateWith CommitPage CommitMsg model

        ( Ports.JobStatsUpdate event, BuildsPage m ) ->
            ( { model | page = BuildsPage (Builds.websocketUpdate event m) }, Cmd.none )

        ( Ports.LogLine event, DrvPage m ) ->
            Drv.update (Drv.LogLineReceived event) m |> updateWith DrvPage DrvMsg model

        _ ->
            ( model, Cmd.none )



-- HELPERS


{-| Backend routes (like the OAuth endpoints) need a full page load.
-}
isBackendRoute : Url -> Bool
isBackendRoute url =
    String.startsWith "/github/auth/" url.path


{-| Unsubscribe from WebSocket resources when leaving a page.
-}
unsubscribeFromPage : Page -> Cmd Msg
unsubscribeFromPage page =
    case page of
        BuildsPage m ->
            Builds.cleanup m

        JobPage m ->
            Ports.websocketOut (Ports.encodeUnsubscribeMessage "job" (String.fromInt m.jobsetId))

        CommitPage m ->
            Ports.websocketOut (Ports.encodeUnsubscribeMessage "commit" m.sha)

        DrvPage m ->
            Ports.websocketOut (Ports.encodeUnsubscribeMessage "drv" m.drvPath)

        _ ->
            Cmd.none


fetchUserInfo : String -> Cmd Msg
fetchUserInfo token =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ token) ]
        , url = Auth.getUserInfo
        , body = Http.emptyBody
        , expect = Http.expectJson (GotUserInfo token) Auth.decodeUser
        , timeout = Nothing
        , tracker = Nothing
        }



-- SUBSCRIPTIONS


subscriptions : Model -> Sub Msg
subscriptions model =
    Sub.batch
        [ Ports.websocketIn handleWebSocketMessage
        , Ports.tokenReceived TokenReceived
        , case model.page of
            BuildsPage m ->
                Sub.map BuildsMsg (Builds.subscriptions m)

            _ ->
                Sub.none
        ]


handleWebSocketMessage : E.Value -> Msg
handleWebSocketMessage value =
    case Ports.decodeIncomingMessage value of
        Ok message ->
            WebSocketMessage message

        Err _ ->
            NoOp



-- VIEW


view : Model -> Browser.Document Msg
view model =
    { title = pageTitle model.route ++ " · EkaCI"
    , body =
        [ a [ class "skip-link", href "#main" ] [ text "Skip to content" ]
        , Header.view
            { route = model.route
            , authState = model.authState
            , live = model.live
            , theme = model.theme
            , onToggleTheme = ThemeToggled
            , onLogout = LogoutRequested
            }
        , main_ [ id "main", class "page", tabindex -1 ] [ viewPage model ]
        ]
    }


pageTitle : Route -> String
pageTitle route =
    case route of
        Route.Home ->
            "Repositories"

        Route.Builds ->
            "Active builds"

        Route.Reviews ->
            "Reviews"

        Route.PullRequest _ _ _ ->
            "Page not found"

        Route.Repository owner repo ->
            owner ++ "/" ++ repo

        Route.Commit sha ->
            "Commit " ++ Ui.shortSha sha

        Route.Job jobsetId ->
            "Job #" ++ String.fromInt jobsetId

        Route.Drv drvPath ->
            Ui.drvName drvPath

        Route.Admin ->
            "Admin"

        Route.Profile ->
            "Profile"

        Route.AttrPathSearch ->
            "Maintainers"

        Route.AuthCallback ->
            "Signing in"

        Route.NotFound ->
            "Page not found"


viewPage : Model -> Html Msg
viewPage model =
    case model.page of
        HomePage m ->
            Html.map HomeMsg (Home.view m)

        BuildsPage m ->
            Html.map BuildsMsg (Builds.view m)

        ReviewsPage m ->
            Html.map ReviewsMsg (Reviews.view m)

        RepositoryPage m ->
            Html.map RepositoryMsg (Repository.view m)

        CommitPage m ->
            Html.map CommitMsg (Commit.view m)

        JobPage m ->
            Html.map JobMsg (Job.view m)

        DrvPage m ->
            Html.map DrvMsg (Drv.view m)

        AdminPage m ->
            Html.map AdminMsg (Admin.view m)

        ProfilePage m ->
            Html.map ProfileMsg (Profile.view m)

        AttrPathSearchPage m ->
            Html.map AttrPathSearchMsg (AttrPathSearch.view m)

        AuthCallbackPage m ->
            Html.map AuthCallbackMsg (AuthCallback.view m)

        AuthRequiredPage ->
            viewAuthRequired model.authState

        ForbiddenPage ->
            Ui.errorPage
                { code = "403"
                , heading = "Admins only"
                , body = "You're signed in, but this page is only available to EkaCI admins."
                , action = a [ class "button", href (Route.toHref Route.Home) ] [ text "Go to repositories" ]
                }

        NotFoundPage ->
            Ui.errorPage
                { code = "404"
                , heading = "Page not found"
                , body = "There's nothing at this address. The link may be wrong, or the page may have moved."
                , action = a [ class "button", href (Route.toHref Route.Home) ] [ text "Go to repositories" ]
                }


viewAuthRequired : AuthState -> Html Msg
viewAuthRequired authState =
    if authState.loading then
        Ui.loading "Checking your sign-in…"

    else
        Ui.errorPage
            { code = ""
            , heading = "Sign in to continue"
            , body = "This page shows your account, so you need to sign in with GitHub first."
            , action = a [ class "button primary", href Auth.login ] [ text "Sign in with GitHub" ]
            }

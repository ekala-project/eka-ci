module Components.Header exposing
    ( Config
    , Live(..)
    , view
    )

{-| Site header: brand, main navigation (with the current section marked),
live-update status, theme toggle and the signed-in user.
-}

import Auth exposing (AuthState)
import Components.Ui as Ui
import Html exposing (Html, a, button, div, header, nav, span, text)
import Html.Attributes exposing (attribute, class, href, title, type_)
import Html.Events exposing (onClick)
import Route exposing (Route)
import Theme exposing (Theme)


{-| State of the shared WebSocket used for live updates.
-}
type Live
    = Connecting
    | Connected
    | Offline


type alias Config msg =
    { route : Route
    , authState : AuthState
    , live : Live
    , theme : Theme
    , onToggleTheme : msg
    , onLogout : msg
    }


view : Config msg -> Html msg
view config =
    header [ class "site-header" ]
        [ div [ class "site-header-inner" ]
            [ a [ class "brand", href (Route.toHref Route.Home) ] [ text "EkaCI" ]
            , nav [ class "site-nav", attribute "aria-label" "Main" ]
                (List.map (navLink config.route) (navItems config.authState))
            , div [ class "header-tools" ]
                [ liveStatus config.live
                , themeToggle config.theme config.onToggleTheme
                , userArea config
                ]
            ]
        ]



-- NAVIGATION


type Section
    = Repositories
    | Builds
    | Reviews
    | Maintainers
    | Admin
    | NoSection


navItems : AuthState -> List ( Section, String, Route )
navItems authState =
    [ ( Repositories, "Repositories", Route.Home )
    , ( Builds, "Builds", Route.Builds )
    , ( Reviews, "Reviews", Route.Reviews )
    , ( Maintainers, "Maintainers", Route.AttrPathSearch )
    ]
        ++ (if Auth.isAdmin authState then
                [ ( Admin, "Admin", Route.Admin ) ]

            else
                []
           )


{-| Which nav item a route belongs to: drill-down pages (repository, commit,
job, derivation) stay under Repositories.
-}
sectionOf : Route -> Section
sectionOf route =
    case route of
        Route.Home ->
            Repositories

        Route.Repository _ _ ->
            Repositories

        Route.Commit _ ->
            Repositories

        Route.Job _ ->
            Repositories

        Route.Drv _ ->
            Repositories

        Route.PullRequest _ _ _ ->
            Reviews

        Route.Builds ->
            Builds

        Route.Reviews ->
            Reviews

        Route.AttrPathSearch ->
            Maintainers

        Route.Admin ->
            Admin

        Route.Profile ->
            NoSection

        Route.AuthCallback ->
            NoSection

        Route.NotFound ->
            NoSection


navLink : Route -> ( Section, String, Route ) -> Html msg
navLink current ( section, label, route ) =
    a
        (href (Route.toHref route)
            :: (if sectionOf current == section then
                    [ attribute "aria-current" "page" ]

                else
                    []
               )
        )
        [ text label ]



-- TOOLS


liveStatus : Live -> Html msg
liveStatus live =
    let
        ( state, label, tip ) =
            case live of
                Connecting ->
                    ( "connecting", "Connecting", "Connecting to live updates…" )

                Connected ->
                    ( "live", "Live", "Live updates connected" )

                Offline ->
                    ( "offline", "Offline", "Live updates disconnected — reconnecting automatically. Pages still load, but won't update on their own." )
    in
    span
        [ class "live-status"
        , attribute "data-state" state
        , attribute "role" "status"
        , title tip
        ]
        [ span [ class "dot", attribute "aria-hidden" "true" ] []
        , span [ class "live-label" ] [ text label ]
        ]


themeToggle : Theme -> msg -> Html msg
themeToggle theme onToggle =
    let
        nextLabel =
            Theme.label (Theme.next theme)
    in
    button
        [ type_ "button"
        , class "theme-toggle"
        , attribute "data-mode" (Theme.toString theme)
        , attribute "aria-label" ("Theme: " ++ Theme.label theme ++ ". Switch to " ++ nextLabel)
        , title ("Theme: " ++ Theme.label theme ++ " (click for " ++ nextLabel ++ ")")
        , onClick onToggle
        ]
        [ span [ class "theme-icon", attribute "aria-hidden" "true" ] []
        , span [ class "theme-label" ] [ text (Theme.label theme) ]
        ]


userArea : Config msg -> Html msg
userArea config =
    case ( config.authState.user, config.authState.loading ) of
        ( Just user, _ ) ->
            div [ class "site-user" ]
                [ a
                    ([ class "user-link"
                     , href (Route.toHref Route.Profile)
                     , title "Your profile"
                     ]
                        ++ (if config.route == Route.Profile then
                                [ attribute "aria-current" "page" ]

                            else
                                []
                           )
                    )
                    [ Ui.avatar user.avatarUrl user.username
                    , span [ class "user-name" ] [ text user.username ]
                    ]
                , button [ type_ "button", class "button ghost small", onClick config.onLogout ] [ text "Log out" ]
                ]

        ( Nothing, True ) ->
            -- Still resolving a stored session; avoid flashing "Sign in".
            text ""

        ( Nothing, False ) ->
            a [ class "button small", href Auth.login ] [ text "Sign in with GitHub" ]

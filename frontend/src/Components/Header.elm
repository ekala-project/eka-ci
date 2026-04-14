module Components.Header exposing (view)

{-| Application header with navigation and authentication.

Provides a consistent header across all pages with branding, navigation links,
and login/logout functionality.

-}

import Auth exposing (AuthState)
import Html exposing (Html, a, button, div, h1, header, img, nav, span, text)
import Html.Attributes exposing (alt, class, href, src)
import Html.Events exposing (onClick)
import Route exposing (Route)


{-| View the application header with authentication state.

Shows login button when not authenticated, user info and logout when authenticated.

-}
view : AuthState -> msg -> Html msg
view authState onLogout =
    header [ class "app-header" ]
        [ div [ class "flex items-center justify-between" ]
            [ -- Logo and branding
              div [ class "flex items-center" ]
                [ a
                    [ href (Route.toHref Route.Home)
                    , class "flex items-center"
                    ]
                    [ h1 [ class "f3 fw6 ma0" ]
                        [ text "EkaCI" ]
                    ]
                ]

            -- Navigation and auth
            , nav [ class "flex items-center" ]
                [ a
                    [ href (Route.toHref Route.Home)
                    , class "mh3 f6 fw5 link white hover-white-80"
                    ]
                    [ text "Repositories" ]
                , a
                    [ href (Route.toHref Route.Builds)
                    , class "mh3 f6 fw5 link white hover-white-80"
                    ]
                    [ text "Builds" ]
                , a
                    [ href (Route.toHref Route.Reviews)
                    , class "mh3 f6 fw5 link white hover-white-80"
                    ]
                    [ text "Reviews" ]
                , a
                    [ href (Route.toHref Route.AttrPathSearch)
                    , class "mh3 f6 fw5 link white hover-white-80"
                    ]
                    [ text "Attr Paths" ]

                -- Auth section
                , viewAuthSection authState onLogout
                ]
            ]
        ]


{-| View authentication section (login button or user menu).
-}
viewAuthSection : AuthState -> msg -> Html msg
viewAuthSection authState onLogout =
    case authState.user of
        Nothing ->
            -- Not authenticated: show login button
            a
                [ href "/github/auth/login"
                , class "ml3 pa2 br2 bg-white-20 hover-bg-white-30 link white fw5 f6"
                ]
                [ text "Login with GitHub" ]

        Just user ->
            -- Authenticated: show user info and logout
            div [ class "flex items-center ml3" ]
                [ -- Show Admin link for admin users
                  if user.isAdmin then
                    a
                        [ href (Route.toHref Route.Admin)
                        , class "mr3 link white hover-white-80 f6"
                        ]
                        [ text "Admin" ]

                  else
                    text ""

                -- Profile link
                , a
                    [ href (Route.toHref Route.Profile)
                    , class "mr3 link white hover-white-80 f6"
                    ]
                    [ text "Profile" ]

                -- User avatar
                , img
                    [ src user.avatarUrl
                    , alt user.username
                    , class "br-100 w2 h2 mr2"
                    ]
                    []

                -- Username
                , span [ class "mr3 f6 white" ]
                    [ text user.username ]

                -- Logout button
                , button
                    [ onClick onLogout
                    , class "pa2 br2 bn bg-white-20 hover-bg-white-30 white pointer f6"
                    ]
                    [ text "Logout" ]
                ]

module Components.Header exposing (view)

{-| Application header with navigation.

Provides a consistent header across all pages with branding and navigation links.

-}

import Html exposing (Html, a, div, header, h1, nav, text)
import Html.Attributes exposing (class, href)
import Route exposing (Route)


{-| View the application header.

    view
    --> header with EkaCI branding and navigation

-}
view : Html msg
view =
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

            -- Navigation links
            , nav [ class "flex items-center" ]
                [ a
                    [ href (Route.toHref Route.Home)
                    , class "mh3 f6 fw5"
                    ]
                    [ text "Repositories" ]
                ]
            ]
        ]

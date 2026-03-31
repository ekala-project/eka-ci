module Pages.Admin exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Admin page (placeholder for future functionality).

Future features might include:

  - System settings
  - User management
  - Build worker status
  - System metrics and monitoring

-}

import Html exposing (Html, div, h1, p, text)
import Html.Attributes exposing (class)


{-| Page model.
-}
type alias Model =
    { apiBaseUrl : String
    }


{-| Page messages.
-}
type Msg
    = NoOp


{-| Initialize the page.
-}
init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( { apiBaseUrl = apiBaseUrl }, Cmd.none )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        NoOp ->
            ( model, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div [ class "pa4" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Admin" ]
        , div [ class "card pa4" ]
            [ p [ class "gray f5" ]
                [ text "Admin functionality coming soon..." ]
            , p [ class "gray f6 mt3" ]
                [ text "Future features:" ]
            , div [ class "ml3 mt2 gray f6" ]
                [ p [] [ text "• System settings and configuration" ]
                , p [] [ text "• User management" ]
                , p [] [ text "• Build worker status and metrics" ]
                , p [] [ text "• System monitoring and health checks" ]
                ]
            ]
        ]

module Components.Loader exposing (view, viewLarge, viewWithMessage)

{-| Loading spinner component.

Displays an animated loading spinner to indicate ongoing operations.

-}

import Html exposing (Html, div, p, text)
import Html.Attributes exposing (class)


{-| View a standard loading spinner.

    view
    --> <div class="spinner"></div>

-}
view : Html msg
view =
    div [ class "spinner" ] []


{-| View a large loading spinner.

    viewLarge
    --> <div class="spinner spinner-large"></div>

-}
viewLarge : Html msg
viewLarge =
    div [ class "spinner spinner-large" ] []


{-| View a loading spinner with a message.

    viewWithMessage "Loading repositories..."
    --> Centered spinner with message below

-}
viewWithMessage : String -> Html msg
viewWithMessage message =
    div [ class "flex flex-column items-center justify-center pv5" ]
        [ viewLarge
        , p [ class "mt3 gray f5" ]
            [ text message ]
        ]

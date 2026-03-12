module Components.ErrorView exposing (view, viewHttp)

{-| Error display component.

Displays error messages in a consistent, user-friendly format.

-}

import Html exposing (Html, code, div, h3, p, text)
import Html.Attributes exposing (class)
import Http


{-| View a simple error message.

    view "Failed to load data"
    --> Error container with the message

-}
view : String -> Html msg
view message =
    div [ class "error-container" ]
        [ h3 [ class "error-title" ]
            [ text "Error" ]
        , p [ class "error-message" ]
            [ text message ]
        ]


{-| View an HTTP error with appropriate message.

    viewHttp (Http.BadStatus 404)
    --> Error container explaining the 404 error

-}
viewHttp : Http.Error -> Html msg
viewHttp error =
    let
        message =
            case error of
                Http.BadUrl url ->
                    "Invalid URL: " ++ url

                Http.Timeout ->
                    "Request timed out. Please try again."

                Http.NetworkError ->
                    "Network error. Please check your connection."

                Http.BadStatus status ->
                    case status of
                        404 ->
                            "Not found (404). The requested resource does not exist."

                        500 ->
                            "Server error (500). Please try again later."

                        403 ->
                            "Forbidden (403). You don't have permission to access this resource."

                        401 ->
                            "Unauthorized (401). Please authenticate."

                        _ ->
                            "Server error (" ++ String.fromInt status ++ ")."

                Http.BadBody body ->
                    "Failed to parse response: " ++ body
    in
    div [ class "error-container" ]
        [ h3 [ class "error-title" ]
            [ text "Request Failed" ]
        , p [ class "error-message" ]
            [ text message ]
        ]


{-| View an error with details in a code block.
-}
viewWithDetails : String -> String -> Html msg
viewWithDetails title details =
    div [ class "error-container" ]
        [ h3 [ class "error-title" ]
            [ text title ]
        , code [ class "db pa2 bg-light-gray br2 mt2" ]
            [ text details ]
        ]

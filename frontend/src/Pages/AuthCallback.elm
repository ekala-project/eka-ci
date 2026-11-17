module Pages.AuthCallback exposing
    ( Model(..)
    , Msg(..)
    , init
    , update
    , view
    )

{-| GitHub OAuth completion page.

The backend's `/github/auth/callback` verifies the OAuth state, exchanges
the code, and redirects here with the result in the URL fragment:
`/auth/complete#token=<jwt>` or `/auth/complete#error=<message>`.
This page reads the token, fetches the user's profile, and hands both to
Main.elm, which stores the token and redirects home.

-}

import Auth
import Components.Loader as Loader
import Html exposing (Html, div, h1, p, text)
import Html.Attributes exposing (class)
import Http
import Url exposing (Url)


{-| Page model.
-}
type Model
    = Processing
    | Success String Auth.User -- token, user
    | Failed String


{-| Page messages.
-}
type Msg
    = GotUser String (Result Http.Error Auth.User)


{-| Initialize the page from the URL fragment.
-}
init : Url -> ( Model, Cmd Msg )
init url =
    case parseFragment url of
        Just ( "token", token ) ->
            ( Processing, fetchUser token )

        Just ( "error", message ) ->
            ( Failed message, Cmd.none )

        _ ->
            ( Failed "No login result found in URL. The OAuth flow may have been cancelled or failed."
            , Cmd.none
            )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotUser token result ->
            case result of
                Ok user ->
                    -- Success will be handled by Main.elm
                    -- which will store the token and redirect
                    ( Success token user, Cmd.none )

                Err error ->
                    ( Failed (httpErrorToString error)
                    , Cmd.none
                    )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div [ class "pa4 tc" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Authenticating..." ]
        , case model of
            Processing ->
                viewProcessing

            Success _ user ->
                viewSuccess user

            Failed error ->
                viewError error
        ]


{-| View processing state.
-}
viewProcessing : Html Msg
viewProcessing =
    div []
        [ Loader.viewLarge
        , p [ class "gray mt3" ]
            [ text "Completing GitHub authentication..." ]
        ]


{-| View success state.
-}
viewSuccess : Auth.User -> Html Msg
viewSuccess user =
    div [ class "card pa4 tc" ]
        [ div [ class "f3 mb3" ] [ text "✓" ]
        , p [ class "f5 mb2" ]
            [ text ("Welcome, " ++ user.username ++ "!") ]
        , p [ class "gray f6" ]
            [ text "Redirecting to home..." ]
        ]


{-| View error state.
-}
viewError : String -> Html Msg
viewError error =
    div [ class "card pa4" ]
        [ div [ class "error-container" ]
            [ Html.h3 [ class "error-title" ]
                [ text "Authentication Failed" ]
            , p [ class "error-message" ]
                [ text "There was a problem completing the GitHub authentication." ]
            , Html.code [ class "db pa2 bg-light-gray br2 mt2" ]
                [ text error ]
            ]
        ]



{- HELPER FUNCTIONS -}


{-| Parse a single `key=value` pair from the URL fragment.

Values are form-encoded by the backend, so `+` means a space.

-}
parseFragment : Url -> Maybe ( String, String )
parseFragment url =
    url.fragment
        |> Maybe.andThen
            (\fragment ->
                case String.split "=" fragment of
                    key :: rest ->
                        String.join "=" rest
                            |> String.replace "+" " "
                            |> Url.percentDecode
                            |> Maybe.map (Tuple.pair key)

                    [] ->
                        Nothing
            )


{-| Fetch the signed-in user's profile with the new token.
-}
fetchUser : String -> Cmd Msg
fetchUser token =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ token) ]
        , url = Auth.getUserInfo
        , body = Http.emptyBody
        , expect = Http.expectJson (GotUser token) Auth.decodeUser
        , timeout = Nothing
        , tracker = Nothing
        }


{-| Convert HTTP error to user-friendly string.
-}
httpErrorToString : Http.Error -> String
httpErrorToString error =
    case error of
        Http.BadUrl url ->
            "Invalid URL: " ++ url

        Http.Timeout ->
            "Request timed out. Please try again."

        Http.NetworkError ->
            "Network error. Please check your connection and try again."

        Http.BadStatus 401 ->
            "Authentication failed. GitHub may have rejected the authorization."

        Http.BadStatus 403 ->
            "Access forbidden. You may not have permission to access this application."

        Http.BadStatus status ->
            "Server error (status " ++ String.fromInt status ++ "). Please try again."

        Http.BadBody message ->
            "Invalid response from server: " ++ message

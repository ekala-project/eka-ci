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
import Components.Ui as Ui
import Html exposing (Html, a, text)
import Html.Attributes exposing (class, href)
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
    case model of
        Processing ->
            Ui.loading "Finishing sign-in with GitHub…"

        Success _ user ->
            Ui.loading ("Signed in as " ++ user.username ++ ". Taking you back…")

        Failed error ->
            Ui.errorPage
                { code = ""
                , heading = "Could not sign you in"
                , body = error
                , action = a [ class "button primary", href Auth.login ] [ text "Try again" ]
                }



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

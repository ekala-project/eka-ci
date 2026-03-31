module Pages.AuthCallback exposing
    ( Model(..)
    , Msg(..)
    , init
    , update
    , view
    )

{-| GitHub OAuth callback handler page.

This page handles the redirect from GitHub after OAuth authorization.
It extracts the authorization code from the URL query parameters,
exchanges it with the backend for a JWT token, and redirects to home.

-}

import Auth
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Html exposing (Html, div, h1, p, text)
import Html.Attributes exposing (class)
import Http
import Url exposing (Url)
import Url.Parser as UP
import Url.Parser.Query as Query


{-| Page model.
-}
type Model
    = Processing String
    | Success String Auth.User -- token, user
    | Failed String


{-| Page messages.
-}
type Msg
    = GotLoginResponse (Result Http.Error { token : String, user : Auth.User })


{-| Initialize the page and start the OAuth exchange.

Extracts the code and state from the URL query parameters and
makes a request to the backend callback endpoint.

-}
init : String -> Url -> ( Model, Cmd Msg )
init apiBaseUrl url =
    case extractCode url of
        Just code ->
            let
                callbackUrl =
                    buildCallbackUrl apiBaseUrl code (extractState url)
            in
            ( Processing callbackUrl
            , Http.get
                { url = callbackUrl
                , expect = Http.expectJson GotLoginResponse Auth.decodeLoginResponse
                }
            )

        Nothing ->
            ( Failed "No authorization code found in URL. The OAuth flow may have been cancelled or failed."
            , Cmd.none
            )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotLoginResponse result ->
            case result of
                Ok { token, user } ->
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
            Processing _ ->
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


{-| Extract the authorization code from URL query parameters.
-}
extractCode : Url -> Maybe String
extractCode url =
    { url | path = "" }
        |> UP.parse (UP.query (Query.string "code"))
        |> Maybe.withDefault Nothing


{-| Extract the state parameter from URL query parameters.
-}
extractState : Url -> Maybe String
extractState url =
    { url | path = "" }
        |> UP.parse (UP.query (Query.string "state"))
        |> Maybe.withDefault Nothing


{-| Build the callback URL with code and optional state.
-}
buildCallbackUrl : String -> String -> Maybe String -> String
buildCallbackUrl apiBaseUrl code maybeState =
    let
        -- apiBaseUrl is like "http://127.0.0.1:3030/v1"
        -- We need "/github/auth/callback" which is at the root level
        -- So we strip "/v1" and add "/github/auth/callback"
        baseUrl =
            if String.endsWith "/v1" apiBaseUrl then
                String.dropRight 3 apiBaseUrl ++ "/github/auth/callback?code=" ++ code

            else
                apiBaseUrl ++ "/../github/auth/callback?code=" ++ code
    in
    case maybeState of
        Just state ->
            baseUrl ++ "&state=" ++ state

        Nothing ->
            baseUrl


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

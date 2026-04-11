module Pages.Profile exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| User profile page.
-}

import Auth exposing (AuthToken)
import Html exposing (Html, a, button, div, h1, h2, h3, img, li, p, span, text, ul)
import Html.Attributes exposing (class, href, src)
import Html.Events exposing (onClick)
import Http
import Json.Decode as D


{-| Page model.
-}
type alias Model =
    { apiBaseUrl : String
    , authToken : AuthToken
    , profile : Maybe UserProfile
    , loading : Bool
    , error : Maybe String
    }


{-| User profile with maintained paths.
-}
type alias UserProfile =
    { user : UserInfo
    , maintainedPaths : List String
    , createdAt : String
    , lastLogin : String
    }


{-| User information.
-}
type alias UserInfo =
    { githubId : Int
    , username : String
    , avatarUrl : Maybe String
    , isAdmin : Bool
    }


{-| Page messages.
-}
type Msg
    = LoadProfile
    | ProfileLoaded (Result Http.Error UserProfile)


{-| Initialize the page.
-}
init : String -> AuthToken -> ( Model, Cmd Msg )
init apiBaseUrl authToken =
    ( { apiBaseUrl = apiBaseUrl
      , authToken = authToken
      , profile = Nothing
      , loading = True
      , error = Nothing
      }
    , loadProfile apiBaseUrl authToken
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        LoadProfile ->
            ( { model | loading = True, error = Nothing }
            , loadProfile model.apiBaseUrl model.authToken
            )

        ProfileLoaded (Ok profile) ->
            ( { model | profile = Just profile, loading = False }
            , Cmd.none
            )

        ProfileLoaded (Err err) ->
            ( { model
                | loading = False
                , error = Just (httpErrorToString err)
              }
            , Cmd.none
            )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div [ class "pa4 center mw7" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "My Profile" ]
        , if model.loading then
            div [ class "card pa4 tc" ]
                [ p [ class "gray" ] [ text "Loading profile..." ]
                ]

          else
            case model.error of
                Just err ->
                    div [ class "card pa4 bg-light-red" ]
                        [ p [ class "dark-red" ] [ text ("Error: " ++ err) ]
                        , button
                            [ onClick LoadProfile
                            , class "btn-primary mt3"
                            ]
                            [ text "Retry" ]
                        ]

                Nothing ->
                    case model.profile of
                        Just profile ->
                            viewProfile profile

                        Nothing ->
                            div [ class "card pa4 tc gray" ]
                                [ p [] [ text "Profile not found" ] ]
        ]


viewProfile : UserProfile -> Html Msg
viewProfile profile =
    div []
        [ div [ class "card pa4 mb4" ]
            [ div [ class "flex items-center gap3" ]
                [ case profile.user.avatarUrl of
                    Just url ->
                        img
                            [ src url
                            , class "br-100 w4 h4"
                            ]
                            []

                    Nothing ->
                        div [ class "br-100 w4 h4 bg-light-gray" ] []
                , div []
                    [ h2 [ class "f3 fw6 ma0" ] [ text profile.user.username ]
                    , div [ class "flex gap2 mt2" ]
                        [ span [ class "badge badge-default" ]
                            [ text ("ID: " ++ String.fromInt profile.user.githubId) ]
                        , if profile.user.isAdmin then
                            span [ class "badge badge-success" ]
                                [ text "Admin" ]

                          else
                            text ""
                        ]
                    , a
                        [ href ("https://github.com/" ++ profile.user.username)
                        , class "link blue dim f6 mt2 dib"
                        ]
                        [ text "View on GitHub →" ]
                    ]
                ]
            ]
        , div [ class "card pa4 mb4" ]
            [ h3 [ class "f4 fw6 mb3" ] [ text "Account Information" ]
            , div [ class "mb2" ]
                [ span [ class "gray" ] [ text "Member since: " ]
                , span [] [ text (formatDate profile.createdAt) ]
                ]
            , div []
                [ span [ class "gray" ] [ text "Last login: " ]
                , span [] [ text (formatDate profile.lastLogin) ]
                ]
            ]
        , div [ class "card pa4" ]
            [ h3 [ class "f4 fw6 mb3" ] [ text "Maintained Attribute Paths" ]
            , if List.isEmpty profile.maintainedPaths then
                p [ class "gray" ] [ text "You are not maintaining any attribute paths yet." ]

              else
                ul [ class "list pl0" ]
                    (List.map viewMaintainedPath profile.maintainedPaths)
            ]
        ]


viewMaintainedPath : String -> Html Msg
viewMaintainedPath path =
    li [ class "bb b--light-gray pv2" ]
        [ span [ class "code f6" ] [ text path ] ]



-- API CALLS


loadProfile : String -> AuthToken -> Cmd Msg
loadProfile apiBaseUrl authToken =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/users/me/profile"
        , body = Http.emptyBody
        , expect = Http.expectJson ProfileLoaded profileDecoder
        , timeout = Nothing
        , tracker = Nothing
        }



-- DECODERS


userInfoDecoder : D.Decoder UserInfo
userInfoDecoder =
    D.map4 UserInfo
        (D.field "github_id" D.int)
        (D.field "username" D.string)
        (D.field "avatar_url" (D.nullable D.string))
        (D.field "is_admin" D.bool)


profileDecoder : D.Decoder UserProfile
profileDecoder =
    D.map4 UserProfile
        (D.field "user" userInfoDecoder)
        (D.field "maintained_paths" (D.list D.string))
        (D.field "created_at" D.string)
        (D.field "last_login" D.string)



-- HELPERS


httpErrorToString : Http.Error -> String
httpErrorToString error =
    case error of
        Http.BadUrl url ->
            "Invalid URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus code ->
            "Server returned error code: " ++ String.fromInt code

        Http.BadBody body ->
            "Invalid response: " ++ body


formatDate : String -> String
formatDate dateStr =
    -- For now, just return the date string as-is
    -- In a real app, you'd parse and format this nicely
    dateStr

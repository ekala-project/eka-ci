module Pages.AttrPathSearch exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Attribute path search page.

Allows users to search for attr paths and view their maintainers.

-}

import Api.Api as Api
import Auth exposing (AuthState, AuthToken)
import Html exposing (Html, a, button, div, h1, h2, h3, img, input, li, p, span, text, ul)
import Html.Attributes exposing (class, disabled, href, placeholder, src, type_, value)
import Html.Events exposing (onClick, onInput)
import Http
import Models.Maintainer exposing (MaintainerDetail)


{-| Page model.
-}
type alias Model =
    { apiBaseUrl : String
    , authState : AuthState
    , searchQuery : String
    , searchResults : List MaintainerDetail
    , searching : Bool
    , searched : Bool
    , error : Maybe String
    , requestingAccess : Bool
    , successMessage : Maybe String
    }


{-| Page messages.
-}
type Msg
    = SetSearchQuery String
    | Search
    | SearchCompleted (Result Http.Error (List MaintainerDetail))
    | RequestMaintainerAccess String
    | MaintainerRequestCompleted (Result Http.Error ())
    | DismissMessage


{-| Initialize the page.
-}
init : String -> AuthState -> ( Model, Cmd Msg )
init apiBaseUrl authState =
    ( { apiBaseUrl = apiBaseUrl
      , authState = authState
      , searchQuery = ""
      , searchResults = []
      , searching = False
      , searched = False
      , error = Nothing
      , requestingAccess = False
      , successMessage = Nothing
      }
    , Cmd.none
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        SetSearchQuery query ->
            ( { model | searchQuery = query }
            , Cmd.none
            )

        Search ->
            if String.isEmpty (String.trim model.searchQuery) then
                ( { model | error = Just "Please enter an attr path to search" }
                , Cmd.none
                )

            else
                ( { model
                    | searching = True
                    , searched = False
                    , error = Nothing
                  }
                , Api.getAttrPathMaintainers model.apiBaseUrl model.searchQuery SearchCompleted
                )

        SearchCompleted (Ok results) ->
            ( { model
                | searchResults = results
                , searching = False
                , searched = True
              }
            , Cmd.none
            )

        SearchCompleted (Err err) ->
            ( { model
                | searching = False
                , searched = True
                , error = Just (httpErrorToString err)
              }
            , Cmd.none
            )

        RequestMaintainerAccess attrPath ->
            case Auth.getToken model.authState of
                Just token ->
                    ( { model | requestingAccess = True, error = Nothing }
                    , requestMaintainerAccess model.apiBaseUrl token attrPath
                    )

                Nothing ->
                    ( { model | error = Just "You must be logged in to request access" }
                    , Cmd.none
                    )

        MaintainerRequestCompleted (Ok ()) ->
            ( { model
                | requestingAccess = False
                , successMessage = Just "Maintainer request submitted successfully!"
              }
            , Api.getAttrPathMaintainers model.apiBaseUrl model.searchQuery SearchCompleted
            )

        MaintainerRequestCompleted (Err err) ->
            ( { model
                | requestingAccess = False
                , error = Just (httpErrorToString err)
              }
            , Cmd.none
            )

        DismissMessage ->
            ( { model | error = Nothing, successMessage = Nothing }
            , Cmd.none
            )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    div [ class "pa4 center mw7" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Attribute Path Search" ]
        , viewMessages model
        , div [ class "card pa4 mb4" ]
            [ h2 [ class "f3 fw6 mb3" ] [ text "Search for Maintainers" ]
            , div [ class "flex gap2 mb3" ]
                [ input
                    [ type_ "text"
                    , placeholder "Enter attr path (e.g., nixpkgs.python3)"
                    , value model.searchQuery
                    , onInput SetSearchQuery
                    , class "input flex-auto"
                    , disabled model.searching
                    ]
                    []
                , button
                    [ onClick Search
                    , class "btn-primary"
                    , disabled model.searching
                    ]
                    [ text
                        (if model.searching then
                            "Searching..."

                         else
                            "Search"
                        )
                    ]
                ]
            , p [ class "gray f6" ]
                [ text "Search for an attribute path to view its maintainers and request access." ]
            ]
        , if model.searching then
            div [ class "card pa4 tc" ]
                [ p [ class "gray" ] [ text "Searching..." ] ]

          else if model.searched then
            viewSearchResults model

          else
            div [ class "card pa4 tc gray" ]
                [ p [] [ text "Enter an attr path above to search for maintainers." ] ]
        ]


viewMessages : Model -> Html Msg
viewMessages model =
    div []
        [ case model.error of
            Just err ->
                div [ class "card pa3 mb3 bg-light-red" ]
                    [ div [ class "flex justify-between items-center" ]
                        [ span [ class "dark-red" ] [ text ("Error: " ++ err) ]
                        , button
                            [ onClick DismissMessage
                            , class "bn bg-transparent pointer dark-red"
                            ]
                            [ text "×" ]
                        ]
                    ]

            Nothing ->
                text ""
        , case model.successMessage of
            Just msg ->
                div [ class "card pa3 mb3 bg-light-green" ]
                    [ div [ class "flex justify-between items-center" ]
                        [ span [ class "dark-green" ] [ text msg ]
                        , button
                            [ onClick DismissMessage
                            , class "bn bg-transparent pointer dark-green"
                            ]
                            [ text "×" ]
                        ]
                    ]

            Nothing ->
                text ""
        ]


viewSearchResults : Model -> Html Msg
viewSearchResults model =
    div [ class "card pa4" ]
        [ div [ class "flex justify-between items-center mb3" ]
            [ h2 [ class "f3 fw6" ]
                [ text ("Results for: " ++ model.searchQuery) ]
            , viewRequestButton model
            ]
        , if List.isEmpty model.searchResults then
            div []
                [ p [ class "gray mb3" ] [ text "No maintainers found for this attr path." ]
                , p [ class "f6 gray" ]
                    [ text "This attr path has no maintainers yet. You can be the first to request access!" ]
                ]

          else
            div []
                [ p [ class "f6 mb3 gray" ]
                    [ text (String.fromInt (List.length model.searchResults) ++ " maintainer(s)") ]
                , ul [ class "list pl0" ]
                    (List.map viewMaintainer model.searchResults)
                ]
        ]


viewMaintainer : MaintainerDetail -> Html Msg
viewMaintainer maintainer =
    li [ class "bb b--light-gray pv3" ]
        [ div [ class "flex items-center gap3" ]
            [ case maintainer.githubAvatarUrl of
                Just url ->
                    img
                        [ src url
                        , class "br-100 w3 h3"
                        ]
                        []

                Nothing ->
                    div [ class "br-100 w3 h3 bg-light-gray" ] []
            , div [ class "flex-auto" ]
                [ div [ class "fw6 mb1" ]
                    [ a
                        [ href ("https://github.com/" ++ maintainer.githubUsername)
                        , class "link blue dim"
                        ]
                        [ text maintainer.githubUsername ]
                    ]
                , div [ class "f6 gray" ]
                    [ text ("Added: " ++ formatDate maintainer.addedAt)
                    , case ( maintainer.addedByUserId, maintainer.addedByUsername ) of
                        ( Just _, Just addedBy ) ->
                            text (" by " ++ addedBy)

                        _ ->
                            text ""
                    ]
                ]
            ]
        ]


viewRequestButton : Model -> Html Msg
viewRequestButton model =
    let
        currentUserId =
            model.authState.user
                |> Maybe.map .githubId

        isAlreadyMaintainer =
            List.any
                (\m -> Just m.githubUserId == currentUserId)
                model.searchResults
    in
    if not (Auth.isAuthenticated model.authState) then
        a
            [ href Auth.login
            , class "btn-primary"
            ]
            [ text "Log in to request access" ]

    else if isAlreadyMaintainer then
        button
            [ class "btn-secondary"
            , disabled True
            ]
            [ text "You are a maintainer" ]

    else if model.requestingAccess then
        button
            [ class "btn-primary"
            , disabled True
            ]
            [ text "Requesting..." ]

    else
        button
            [ onClick (RequestMaintainerAccess model.searchQuery)
            , class "btn-primary"
            ]
            [ text "Request Maintainer Access" ]



-- API CALLS


requestMaintainerAccess : String -> AuthToken -> String -> Cmd Msg
requestMaintainerAccess apiBaseUrl authToken attrPath =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/attr-paths/" ++ attrPath ++ "/request-maintainer"
        , body = Http.emptyBody
        , expect = Http.expectWhatever MaintainerRequestCompleted
        , timeout = Nothing
        , tracker = Nothing
        }



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

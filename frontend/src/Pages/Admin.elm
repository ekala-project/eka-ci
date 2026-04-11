module Pages.Admin exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Admin page with user and maintainer management.
-}

import Auth exposing (AuthToken)
import Html exposing (Html, button, div, h1, h2, h3, input, li, p, span, table, tbody, td, text, th, thead, tr, ul)
import Html.Attributes exposing (class, disabled, placeholder, type_, value)
import Html.Events exposing (onClick, onInput)
import Http
import Json.Decode as D
import Json.Encode as E


{-| Page model.
-}
type alias Model =
    { apiBaseUrl : String
    , authToken : AuthToken
    , users : List User
    , selectedUser : Maybe User
    , maintainers : List Maintainer
    , loadingUsers : Bool
    , loadingMaintainers : Bool
    , error : Maybe String
    , successMessage : Maybe String
    , newMaintainerPath : String
    , newMaintainerUserId : String
    , selectedAttrPath : Maybe String
    }


{-| User detail from admin API.
-}
type alias User =
    { githubId : Int
    , githubUsername : String
    , githubAvatarUrl : Maybe String
    , isAdmin : Bool
    , createdAt : String
    , lastLogin : String
    }


{-| Attr path maintainer.
-}
type alias Maintainer =
    { id : Int
    , attrPath : String
    , githubUserId : Int
    , addedAt : String
    , addedByUserId : Maybe Int
    }


{-| Page messages.
-}
type Msg
    = LoadUsers
    | UsersLoaded (Result Http.Error (List User))
    | PromoteUser Int
    | DemoteUser Int
    | DeleteUser Int
    | UserActionCompleted (Result Http.Error ())
    | SelectUser (Maybe User)
    | LoadMaintainersForPath String
    | MaintainersLoaded (Result Http.Error (List Maintainer))
    | SetNewMaintainerPath String
    | SetNewMaintainerUserId String
    | AddMaintainer
    | RemoveMaintainer String Int
    | MaintainerActionCompleted (Result Http.Error ())
    | DismissMessage


{-| Initialize the page.
-}
init : String -> AuthToken -> ( Model, Cmd Msg )
init apiBaseUrl authToken =
    ( { apiBaseUrl = apiBaseUrl
      , authToken = authToken
      , users = []
      , selectedUser = Nothing
      , maintainers = []
      , loadingUsers = True
      , loadingMaintainers = False
      , error = Nothing
      , successMessage = Nothing
      , newMaintainerPath = ""
      , newMaintainerUserId = ""
      , selectedAttrPath = Nothing
      }
    , loadUsers apiBaseUrl authToken
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        LoadUsers ->
            ( { model | loadingUsers = True, error = Nothing }
            , loadUsers model.apiBaseUrl model.authToken
            )

        UsersLoaded (Ok users) ->
            ( { model | users = users, loadingUsers = False }
            , Cmd.none
            )

        UsersLoaded (Err err) ->
            ( { model
                | loadingUsers = False
                , error = Just (httpErrorToString err)
              }
            , Cmd.none
            )

        PromoteUser githubId ->
            ( { model | error = Nothing }
            , promoteUser model.apiBaseUrl model.authToken githubId
            )

        DemoteUser githubId ->
            ( { model | error = Nothing }
            , demoteUser model.apiBaseUrl model.authToken githubId
            )

        DeleteUser githubId ->
            ( { model | error = Nothing }
            , deleteUser model.apiBaseUrl model.authToken githubId
            )

        UserActionCompleted (Ok ()) ->
            ( { model | successMessage = Just "Action completed successfully" }
            , loadUsers model.apiBaseUrl model.authToken
            )

        UserActionCompleted (Err err) ->
            ( { model | error = Just (httpErrorToString err) }
            , Cmd.none
            )

        SelectUser user ->
            ( { model | selectedUser = user }
            , Cmd.none
            )

        LoadMaintainersForPath attrPath ->
            ( { model
                | loadingMaintainers = True
                , error = Nothing
                , selectedAttrPath = Just attrPath
              }
            , loadMaintainers model.apiBaseUrl model.authToken attrPath
            )

        MaintainersLoaded (Ok maintainers) ->
            ( { model | maintainers = maintainers, loadingMaintainers = False }
            , Cmd.none
            )

        MaintainersLoaded (Err err) ->
            ( { model
                | loadingMaintainers = False
                , error = Just (httpErrorToString err)
              }
            , Cmd.none
            )

        SetNewMaintainerPath path ->
            ( { model | newMaintainerPath = path }
            , Cmd.none
            )

        SetNewMaintainerUserId userId ->
            ( { model | newMaintainerUserId = userId }
            , Cmd.none
            )

        AddMaintainer ->
            case ( model.newMaintainerPath, String.toInt model.newMaintainerUserId ) of
                ( "", _ ) ->
                    ( { model | error = Just "Please enter an attr path" }
                    , Cmd.none
                    )

                ( _, Nothing ) ->
                    ( { model | error = Just "Please enter a valid user ID" }
                    , Cmd.none
                    )

                ( path, Just userId ) ->
                    ( { model | error = Nothing }
                    , addMaintainer model.apiBaseUrl model.authToken path userId
                    )

        RemoveMaintainer attrPath githubId ->
            ( { model | error = Nothing }
            , removeMaintainer model.apiBaseUrl model.authToken attrPath githubId
            )

        MaintainerActionCompleted (Ok ()) ->
            ( { model
                | successMessage = Just "Maintainer action completed successfully"
                , newMaintainerPath = ""
                , newMaintainerUserId = ""
              }
            , case model.selectedAttrPath of
                Just path ->
                    loadMaintainers model.apiBaseUrl model.authToken path

                Nothing ->
                    Cmd.none
            )

        MaintainerActionCompleted (Err err) ->
            ( { model | error = Just (httpErrorToString err) }
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
    div [ class "pa4" ]
        [ h1 [ class "f2 fw6 mb4" ]
            [ text "Admin Panel" ]
        , viewMessages model
        , div [ class "flex gap3" ]
            [ div [ class "w-60" ]
                [ viewUsersSection model ]
            , div [ class "w-40" ]
                [ viewMaintainersSection model ]
            ]
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


viewUsersSection : Model -> Html Msg
viewUsersSection model =
    div [ class "card pa4" ]
        [ div [ class "flex justify-between items-center mb3" ]
            [ h2 [ class "f3 fw6" ] [ text "User Management" ]
            , button
                [ onClick LoadUsers
                , class "btn-secondary"
                , disabled model.loadingUsers
                ]
                [ text
                    (if model.loadingUsers then
                        "Loading..."

                     else
                        "Refresh"
                    )
                ]
            ]
        , if model.loadingUsers then
            p [ class "gray" ] [ text "Loading users..." ]

          else if List.isEmpty model.users then
            p [ class "gray" ] [ text "No users found" ]

          else
            table [ class "w-100 collapse" ]
                [ thead []
                    [ tr [ class "bb b--light-gray" ]
                        [ th [ class "tl pv2 ph1" ] [ text "User" ]
                        , th [ class "tl pv2 ph1" ] [ text "GitHub ID" ]
                        , th [ class "tl pv2 ph1" ] [ text "Role" ]
                        , th [ class "tl pv2 ph1" ] [ text "Actions" ]
                        ]
                    ]
                , tbody []
                    (List.map viewUserRow model.users)
                ]
        ]


viewUserRow : User -> Html Msg
viewUserRow user =
    tr [ class "bb b--light-gray" ]
        [ td [ class "pv2 ph1" ]
            [ div [ class "flex items-center gap2" ]
                [ case user.githubAvatarUrl of
                    Just url ->
                        Html.img
                            [ Html.Attributes.src url
                            , class "br-100 w2 h2"
                            , Html.Attributes.alt user.githubUsername
                            ]
                            []

                    Nothing ->
                        div [ class "br-100 w2 h2 bg-light-gray" ] []
                , text user.githubUsername
                ]
            ]
        , td [ class "pv2 ph1 gray f7" ] [ text (String.fromInt user.githubId) ]
        , td [ class "pv2 ph1" ]
            [ span
                [ class
                    (if user.isAdmin then
                        "badge badge-success"

                     else
                        "badge badge-default"
                    )
                ]
                [ text
                    (if user.isAdmin then
                        "Admin"

                     else
                        "User"
                    )
                ]
            ]
        , td [ class "pv2 ph1" ]
            [ div [ class "flex gap2" ]
                [ if user.isAdmin then
                    button
                        [ onClick (DemoteUser user.githubId)
                        , class "btn-sm btn-secondary"
                        ]
                        [ text "Remove Admin" ]

                  else
                    button
                        [ onClick (PromoteUser user.githubId)
                        , class "btn-sm btn-primary"
                        ]
                        [ text "Make Admin" ]
                , button
                    [ onClick (DeleteUser user.githubId)
                    , class "btn-sm btn-danger"
                    ]
                    [ text "Delete" ]
                ]
            ]
        ]


viewMaintainersSection : Model -> Html Msg
viewMaintainersSection model =
    div [ class "card pa4" ]
        [ h2 [ class "f3 fw6 mb3" ] [ text "Attr Path Maintainers" ]
        , div [ class "mb4" ]
            [ h3 [ class "f4 fw6 mb2" ] [ text "Add Maintainer" ]
            , div [ class "mb2" ]
                [ input
                    [ type_ "text"
                    , placeholder "Attr path (e.g., nixpkgs.python3)"
                    , value model.newMaintainerPath
                    , onInput SetNewMaintainerPath
                    , class "input w-100"
                    ]
                    []
                ]
            , div [ class "mb2" ]
                [ input
                    [ type_ "text"
                    , placeholder "GitHub User ID"
                    , value model.newMaintainerUserId
                    , onInput SetNewMaintainerUserId
                    , class "input w-100"
                    ]
                    []
                ]
            , button
                [ onClick AddMaintainer
                , class "btn-primary w-100"
                ]
                [ text "Add Maintainer" ]
            ]
        , div [ class "mb4" ]
            [ h3 [ class "f4 fw6 mb2" ] [ text "Search Maintainers" ]
            , input
                [ type_ "text"
                , placeholder "Attr path to search"
                , class "input w-100"
                , onInput LoadMaintainersForPath
                ]
                []
            ]
        , if model.loadingMaintainers then
            p [ class "gray" ] [ text "Loading maintainers..." ]

          else if List.isEmpty model.maintainers then
            p [ class "gray" ] [ text "No maintainers found for this path" ]

          else
            div []
                [ h3 [ class "f5 fw6 mb2" ]
                    [ text
                        (case model.selectedAttrPath of
                            Just path ->
                                "Maintainers for: " ++ path

                            Nothing ->
                                "Maintainers"
                        )
                    ]
                , ul [ class "list pl0" ]
                    (List.map (viewMaintainer model.selectedAttrPath) model.maintainers)
                ]
        ]


viewMaintainer : Maybe String -> Maintainer -> Html Msg
viewMaintainer selectedPath maintainer =
    li [ class "bb b--light-gray pv2" ]
        [ div [ class "flex justify-between items-center" ]
            [ div []
                [ div [ class "fw6" ] [ text ("User ID: " ++ String.fromInt maintainer.githubUserId) ]
                , div [ class "gray f7 mt1" ] [ text ("Added: " ++ maintainer.addedAt) ]
                ]
            , button
                [ onClick (RemoveMaintainer maintainer.attrPath maintainer.githubUserId)
                , class "btn-sm btn-danger"
                ]
                [ text "Remove" ]
            ]
        ]



-- API CALLS


loadUsers : String -> AuthToken -> Cmd Msg
loadUsers apiBaseUrl authToken =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/users"
        , body = Http.emptyBody
        , expect = Http.expectJson UsersLoaded (D.list userDecoder)
        , timeout = Nothing
        , tracker = Nothing
        }


promoteUser : String -> AuthToken -> Int -> Cmd Msg
promoteUser apiBaseUrl authToken githubId =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/users/" ++ String.fromInt githubId ++ "/promote"
        , body = Http.emptyBody
        , expect = Http.expectWhatever UserActionCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


demoteUser : String -> AuthToken -> Int -> Cmd Msg
demoteUser apiBaseUrl authToken githubId =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/users/" ++ String.fromInt githubId ++ "/demote"
        , body = Http.emptyBody
        , expect = Http.expectWhatever UserActionCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


deleteUser : String -> AuthToken -> Int -> Cmd Msg
deleteUser apiBaseUrl authToken githubId =
    Http.request
        { method = "DELETE"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/users/" ++ String.fromInt githubId
        , body = Http.emptyBody
        , expect = Http.expectWhatever UserActionCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


loadMaintainers : String -> AuthToken -> String -> Cmd Msg
loadMaintainers apiBaseUrl authToken attrPath =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/attr-paths/" ++ attrPath ++ "/maintainers"
        , body = Http.emptyBody
        , expect = Http.expectJson MaintainersLoaded (D.list maintainerDecoder)
        , timeout = Nothing
        , tracker = Nothing
        }


addMaintainer : String -> AuthToken -> String -> Int -> Cmd Msg
addMaintainer apiBaseUrl authToken attrPath githubUserId =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/attr-paths/" ++ attrPath ++ "/maintainers"
        , body =
            Http.jsonBody
                (E.object
                    [ ( "github_user_id", E.int githubUserId )
                    ]
                )
        , expect = Http.expectWhatever MaintainerActionCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


removeMaintainer : String -> AuthToken -> String -> Int -> Cmd Msg
removeMaintainer apiBaseUrl authToken attrPath githubUserId =
    Http.request
        { method = "DELETE"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/v1/admin/attr-paths/" ++ attrPath ++ "/maintainers/" ++ String.fromInt githubUserId
        , body = Http.emptyBody
        , expect = Http.expectWhatever MaintainerActionCompleted
        , timeout = Nothing
        , tracker = Nothing
        }



-- DECODERS


userDecoder : D.Decoder User
userDecoder =
    D.map6 User
        (D.field "github_id" D.int)
        (D.field "github_username" D.string)
        (D.field "github_avatar_url" (D.nullable D.string))
        (D.field "is_admin" D.bool)
        (D.field "created_at" D.string)
        (D.field "last_login" D.string)


maintainerDecoder : D.Decoder Maintainer
maintainerDecoder =
    D.map5 Maintainer
        (D.field "id" D.int)
        (D.field "attr_path" D.string)
        (D.field "github_user_id" D.int)
        (D.field "added_at" D.string)
        (D.field "added_by_user_id" (D.nullable D.int))



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

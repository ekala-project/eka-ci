module Pages.Admin exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Admin: maintainer requests, users and attribute path maintainers.

Every change goes through one `Action` type, so running it, confirming it,
and the success/failure messages are handled in one place. Destructive
actions (remove admin, delete user, remove maintainer) ask for an inline
confirmation first.

-}

import Api.Api as Api
import Api.Decoder as Decoder
import Auth exposing (AuthToken)
import Components.Ui as Ui
import Html exposing (Html, button, div, form, input, label, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (attribute, autocomplete, class, disabled, placeholder, required, scope, spellcheck, type_, value)
import Html.Events exposing (onClick, onInput, onSubmit)
import Http
import Json.Decode as D
import Json.Encode as E
import Models.Maintainer exposing (MaintainerDetail, MaintainerRequest, RequestStatus(..))


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


type Lookup
    = NoLookup
    | LookingUp String
    | LookedUp String (Result Http.Error (List MaintainerDetail))


type alias Model =
    { apiBaseUrl : String
    , authToken : AuthToken
    , users : Remote (List User)
    , requests : Remote (List MaintainerRequest)
    , lookupDraft : String
    , lookup : Lookup
    , newPath : String
    , newUsername : String
    , confirm : Maybe Action
    , working : Bool
    , error : Maybe String
    , success : Maybe String
    }


type alias User =
    { githubId : Int
    , githubUsername : String
    , githubAvatarUrl : Maybe String
    , isAdmin : Bool
    , createdAt : String
    , lastLogin : String
    }


type Action
    = Promote User
    | Demote User
    | DeleteUser User
    | Approve MaintainerRequest
    | Reject MaintainerRequest
    | AddMaintainer String String
    | RemoveMaintainer String MaintainerDetail


type Msg
    = Refresh
    | UsersLoaded (Result Http.Error (List User))
    | RequestsLoaded (Result Http.Error (List MaintainerRequest))
    | SetLookupDraft String
    | SubmitLookup
    | MaintainersLoaded String (Result Http.Error (List MaintainerDetail))
    | SetNewPath String
    | SetNewUsername String
    | SubmitAdd
    | AskConfirm Action
    | CancelConfirm
    | Run Action
    | Done Action (Result Http.Error ())
    | DismissMessage


init : String -> AuthToken -> ( Model, Cmd Msg )
init apiBaseUrl authToken =
    let
        model =
            { apiBaseUrl = apiBaseUrl
            , authToken = authToken
            , users = Loading
            , requests = Loading
            , lookupDraft = ""
            , lookup = NoLookup
            , newPath = ""
            , newUsername = ""
            , confirm = Nothing
            , working = False
            , error = Nothing
            , success = Nothing
            }
    in
    ( model, Cmd.batch [ loadUsers model, loadRequests model ] )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        Refresh ->
            ( model, Cmd.batch [ loadUsers model, loadRequests model ] )

        UsersLoaded result ->
            ( { model | users = toRemote result }, Cmd.none )

        RequestsLoaded result ->
            ( { model | requests = toRemote result }, Cmd.none )

        SetLookupDraft draft ->
            ( { model | lookupDraft = draft }, Cmd.none )

        SubmitLookup ->
            lookUp (String.trim model.lookupDraft) model

        MaintainersLoaded attrPath result ->
            ( { model | lookup = LookedUp attrPath result }, Cmd.none )

        SetNewPath path ->
            ( { model | newPath = path }, Cmd.none )

        SetNewUsername username ->
            ( { model | newUsername = username }, Cmd.none )

        SubmitAdd ->
            submitAdd model

        AskConfirm action ->
            ( { model | confirm = Just action }, Cmd.none )

        CancelConfirm ->
            ( { model | confirm = Nothing }, Cmd.none )

        Run action ->
            ( { model | working = True, confirm = Nothing, error = Nothing, success = Nothing }
            , runAction model action
            )

        Done action result ->
            actionDone action result { model | working = False }

        DismissMessage ->
            ( { model | error = Nothing, success = Nothing }, Cmd.none )


toRemote : Result Http.Error a -> Remote a
toRemote result =
    case result of
        Ok value ->
            Ready value

        Err error ->
            Failed error


lookUp : String -> Model -> ( Model, Cmd Msg )
lookUp attrPath model =
    if String.isEmpty attrPath then
        ( { model | lookup = NoLookup }, Cmd.none )

    else
        ( { model | lookup = LookingUp attrPath, lookupDraft = attrPath }
        , Api.getAttrPathMaintainers model.apiBaseUrl attrPath (MaintainersLoaded attrPath)
        )


submitAdd : Model -> ( Model, Cmd Msg )
submitAdd model =
    let
        path =
            String.trim model.newPath

        username =
            String.trim model.newUsername
                |> (\name ->
                        if String.startsWith "@" name then
                            String.dropLeft 1 name

                        else
                            name
                   )
    in
    if String.isEmpty path || String.isEmpty username then
        ( { model | error = Just "Enter both an attribute path and a GitHub username." }, Cmd.none )

    else
        update (Run (AddMaintainer path username)) model


{-| After an action, refresh whatever it could have changed.
-}
actionDone : Action -> Result Http.Error () -> Model -> ( Model, Cmd Msg )
actionDone action result model =
    case result of
        Err error ->
            ( { model | error = Just (failureText action error) }, Cmd.none )

        Ok () ->
            let
                done =
                    { model | success = Just (successText action) }
            in
            case action of
                AddMaintainer path _ ->
                    lookUp path { done | newUsername = "" }

                RemoveMaintainer path _ ->
                    lookUp path done

                Approve _ ->
                    ( done, loadRequests model )

                Reject _ ->
                    ( done, loadRequests model )

                _ ->
                    ( done, loadUsers model )


successText : Action -> String
successText action =
    case action of
        Promote user ->
            user.githubUsername ++ " is now an admin."

        Demote user ->
            user.githubUsername ++ " is no longer an admin."

        DeleteUser user ->
            "Deleted " ++ user.githubUsername ++ "."

        Approve request ->
            "Approved " ++ request.githubUsername ++ " as a maintainer of " ++ request.attrPath ++ "."

        Reject request ->
            "Rejected " ++ request.githubUsername ++ "’s request for " ++ request.attrPath ++ "."

        AddMaintainer path username ->
            "Added " ++ username ++ " as a maintainer of " ++ path ++ "."

        RemoveMaintainer path maintainer ->
            "Removed " ++ maintainer.githubUsername ++ " from " ++ path ++ "."


failureText : Action -> Http.Error -> String
failureText action error =
    case ( action, error ) of
        ( AddMaintainer _ username, Http.BadStatus 404 ) ->
            "No EkaCI user is called " ++ username ++ ". They need to sign in once before they can be added."

        ( AddMaintainer _ username, Http.BadStatus 409 ) ->
            username ++ " already maintains this attribute path."

        _ ->
            "That didn’t work. " ++ Ui.httpErrorMessage error



-- API


runAction : Model -> Action -> Cmd Msg
runAction model action =
    let
        post path =
            authRequest model "POST" path Http.emptyBody (Http.expectWhatever (Done action))

        delete path =
            authRequest model "DELETE" path Http.emptyBody (Http.expectWhatever (Done action))
    in
    case action of
        Promote user ->
            post ("/admin/users/" ++ String.fromInt user.githubId ++ "/promote")

        Demote user ->
            post ("/admin/users/" ++ String.fromInt user.githubId ++ "/demote")

        DeleteUser user ->
            delete ("/admin/users/" ++ String.fromInt user.githubId)

        Approve request ->
            post ("/admin/maintainer-requests/" ++ String.fromInt request.id ++ "/approve")

        Reject request ->
            post ("/admin/maintainer-requests/" ++ String.fromInt request.id ++ "/reject")

        AddMaintainer path username ->
            authRequest model
                "POST"
                ("/admin/attr-paths/" ++ path ++ "/maintainers/by-username")
                (Http.jsonBody (E.object [ ( "github_username", E.string username ) ]))
                (Http.expectWhatever (Done action))

        RemoveMaintainer path maintainer ->
            delete ("/admin/attr-paths/" ++ path ++ "/maintainers/" ++ String.fromInt maintainer.githubUserId)


loadUsers : Model -> Cmd Msg
loadUsers model =
    authRequest model "GET" "/admin/users" Http.emptyBody (Http.expectJson UsersLoaded (D.list userDecoder))


loadRequests : Model -> Cmd Msg
loadRequests model =
    authRequest model "GET" "/admin/maintainer-requests" Http.emptyBody (Http.expectJson RequestsLoaded Decoder.maintainerRequestList)


authRequest : Model -> String -> String -> Http.Body -> Http.Expect Msg -> Cmd Msg
authRequest model method path body expect =
    Http.request
        { method = method
        , headers = [ Http.header "Authorization" ("Bearer " ++ model.authToken) ]
        , url = model.apiBaseUrl ++ path
        , body = body
        , expect = expect
        , timeout = Nothing
        , tracker = Nothing
        }


userDecoder : D.Decoder User
userDecoder =
    D.map6 User
        (D.field "github_id" D.int)
        (D.field "github_username" D.string)
        (D.field "github_avatar_url" (D.nullable D.string))
        (D.field "is_admin" D.bool)
        (D.field "created_at" D.string)
        (D.field "last_login" D.string)



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ Ui.pageHead
            { crumbs = []
            , title = [ Html.h1 [] [ text "Admin" ] ]
            , lede = [ text "Review maintainer requests and manage users and maintainers." ]
            , actions = [ button [ type_ "button", class "button", onClick Refresh ] [ text "Refresh" ] ]
            }
        , Ui.messages { error = model.error, success = model.success, onDismiss = DismissMessage }
        , viewRequests model
        , viewUsers model
        , div [ class "grid-2" ]
            [ viewLookup model
            , viewAddForm model
            ]
        ]


{-| A destructive action's button, replaced by an inline "are you sure?"
while it is being confirmed.
-}
confirmable : Model -> Action -> { label : String, question : String, verb : String } -> Html Msg
confirmable model action config =
    if model.confirm == Just action then
        span [ class "confirm", attribute "role" "group", attribute "aria-label" config.question ]
            [ span [] [ text config.question ]
            , button [ type_ "button", class "button small danger solid", onClick (Run action), disabled model.working ] [ text config.verb ]
            , button [ type_ "button", class "button small ghost", onClick CancelConfirm ] [ text "Cancel" ]
            ]

    else
        button [ type_ "button", class "button small danger", onClick (AskConfirm action), disabled model.working ] [ text config.label ]


actionButton : Model -> String -> String -> Action -> Html Msg
actionButton model classes label action =
    button [ type_ "button", class ("button small " ++ classes), onClick (Run action), disabled model.working ] [ text label ]



-- REQUESTS


viewRequests : Model -> Html Msg
viewRequests model =
    case model.requests of
        Loading ->
            Ui.panel "Pending requests" [] [ Ui.loadingInline "Loading requests…" ]

        Failed error ->
            Ui.panel "Pending requests" [] [ Ui.httpError "Could not load requests." error ]

        Ready all ->
            let
                pending =
                    List.filter (\r -> r.status == Pending) all
            in
            Ui.panel "Pending requests"
                [ Ui.badge
                    (if List.isEmpty pending then
                        Ui.Neutral

                     else
                        Ui.Warn
                    )
                    (String.fromInt (List.length pending) ++ " pending")
                ]
                [ if List.isEmpty pending then
                    Ui.emptyNote "No requests are waiting for review."

                  else
                    Ui.tableWrap (requestTable model pending)
                ]


requestTable : Model -> List MaintainerRequest -> Html Msg
requestTable model requests =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Request" ]
                , th [ scope "col", class "hide-sm" ] [ text "Requested" ]
                , th [ scope "col" ] [ span [ class "sr-only" ] [ text "Actions" ] ]
                ]
            ]
        , tbody []
            (List.map
                (\request ->
                    tr []
                        [ td []
                            [ span [ class "cell-main mono" ] [ text request.attrPath ]
                            , span [ class "cell-sub request-by" ]
                                [ text "by ", person request.githubAvatarUrl request.githubUsername ]
                            ]
                        , td [ class "nowrap small hide-sm" ] [ Ui.timestamp request.requestedAt ]
                        , td []
                            [ div [ class "actions" ]
                                [ actionButton model "primary" "Approve" (Approve request)
                                , actionButton model "danger" "Reject" (Reject request)
                                ]
                            ]
                        ]
                )
                requests
            )
        ]


person : Maybe String -> String -> Html msg
person avatarUrl username =
    span [ class "person" ] [ Ui.avatar avatarUrl username, text username ]



-- USERS


viewUsers : Model -> Html Msg
viewUsers model =
    Ui.panel "Users"
        []
        [ case model.users of
            Loading ->
                Ui.loadingInline "Loading users…"

            Failed error ->
                Ui.httpError "Could not load users." error

            Ready [] ->
                Ui.emptyNote "Nobody has signed in yet."

            Ready users ->
                Ui.tableWrap (userTable model users)
        ]


userTable : Model -> List User -> Html Msg
userTable model users =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "User" ]
                , th [ scope "col" ] [ text "Role" ]
                , th [ scope "col", class "hide-sm" ] [ text "Last sign-in" ]
                , th [ scope "col" ] [ span [ class "sr-only" ] [ text "Actions" ] ]
                ]
            ]
        , tbody [] (List.map (userRow model) (List.sortBy (\u -> String.toLower u.githubUsername) users))
        ]


userRow : Model -> User -> Html Msg
userRow model user =
    tr []
        [ td []
            [ person user.githubAvatarUrl user.githubUsername
            , span [ class "cell-sub" ] [ text ("GitHub ID " ++ String.fromInt user.githubId) ]
            ]
        , td []
            [ if user.isAdmin then
                Ui.badge Ui.Info "Admin"

              else
                Ui.badge Ui.Neutral "Member"
            ]
        , td [ class "nowrap small hide-sm" ] [ Ui.timestamp user.lastLogin ]
        , td [] [ div [ class "actions" ] (userActions model user) ]
        ]


userActions : Model -> User -> List (Html Msg)
userActions model user =
    [ if user.isAdmin then
        confirmable model
            (Demote user)
            { label = "Remove admin", question = "Remove admin from " ++ user.githubUsername ++ "?", verb = "Remove admin" }

      else
        actionButton model "" "Make admin" (Promote user)
    , confirmable model
        (DeleteUser user)
        { label = "Delete", question = "Delete " ++ user.githubUsername ++ "?", verb = "Delete user" }
    ]



-- MAINTAINERS


viewLookup : Model -> Html Msg
viewLookup model =
    Ui.panel "Maintainers"
        []
        [ form [ class "form-row", onSubmit SubmitLookup, attribute "role" "search" ]
            [ label [ class "field grow" ]
                [ text "Attribute path"
                , input
                    [ type_ "search"
                    , class "mono"
                    , placeholder "e.g. python3Packages.requests"
                    , value model.lookupDraft
                    , onInput SetLookupDraft
                    , autocomplete False
                    , spellcheck False
                    ]
                    []
                ]
            , button [ type_ "submit", class "button" ] [ text "Look up" ]
            ]
        , viewLookupResult model
        ]


viewLookupResult : Model -> Html Msg
viewLookupResult model =
    case model.lookup of
        NoLookup ->
            Ui.emptyNote "Look up an attribute path to see and remove its maintainers."

        LookingUp _ ->
            Ui.loadingInline "Looking up maintainers…"

        LookedUp path (Err error) ->
            Ui.httpError ("Could not look up " ++ path ++ ".") error

        LookedUp path (Ok []) ->
            Ui.emptyNote ("Nobody maintains " ++ path ++ ".")

        LookedUp path (Ok maintainers) ->
            Ui.tableWrap
                (table [ class "table compact" ]
                    [ thead []
                        [ tr []
                            [ th [ scope "col" ] [ text "Maintainer" ]
                            , th [ scope "col" ] [ text "Added" ]
                            , th [ scope "col" ] [ span [ class "sr-only" ] [ text "Actions" ] ]
                            ]
                        ]
                    , tbody [] (List.map (maintainerRow model path) maintainers)
                    ]
                )


maintainerRow : Model -> String -> MaintainerDetail -> Html Msg
maintainerRow model path maintainer =
    tr []
        [ td [] [ person maintainer.githubAvatarUrl maintainer.githubUsername ]
        , td [ class "nowrap small" ] [ Ui.timestamp maintainer.addedAt ]
        , td []
            [ confirmable model
                (RemoveMaintainer path maintainer)
                { label = "Remove", question = "Remove " ++ maintainer.githubUsername ++ "?", verb = "Remove" }
            ]
        ]


viewAddForm : Model -> Html Msg
viewAddForm model =
    Ui.panel "Add a maintainer"
        []
        [ form [ class "form-stack", onSubmit SubmitAdd ]
            [ label [ class "field" ]
                [ text "Attribute path"
                , input [ type_ "text", class "mono", value model.newPath, onInput SetNewPath, required True, autocomplete False, spellcheck False ] []
                ]
            , label [ class "field" ]
                [ text "GitHub username"
                , input [ type_ "text", value model.newUsername, onInput SetNewUsername, required True, autocomplete False, spellcheck False ] []
                , span [ class "field-hint" ] [ text "They must have signed in to EkaCI at least once." ]
                ]
            , div [ class "actions" ]
                [ button [ type_ "submit", class "button primary", disabled model.working ] [ text "Add maintainer" ] ]
            ]
        ]

module Pages.AttrPathSearch exposing
    ( Model
    , Msg
    , init
    , setAuth
    , update
    , view
    )

{-| Maintainers: look up who maintains an attribute path and request access.
-}

import Api.Api as Api
import Auth exposing (AuthState, AuthToken)
import Components.Ui as Ui
import Html exposing (Html, a, button, div, form, input, label, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (attribute, autocomplete, class, disabled, href, placeholder, scope, spellcheck, type_, value)
import Html.Events exposing (onClick, onInput, onSubmit)
import Http
import Models.Maintainer exposing (MaintainerDetail)


type alias Model =
    { apiBaseUrl : String
    , authState : AuthState
    , query : String
    , results : Results
    , requesting : Bool
    , error : Maybe String
    , success : Maybe String
    }


{-| Results always remember the attr path they are for, so editing the
search box doesn't relabel (or redirect requests for) the current results.
-}
type Results
    = NotSearched
    | Searching String
    | Found String (List MaintainerDetail)
    | SearchFailed String Http.Error


type Msg
    = SetQuery String
    | Submit
    | GotMaintainers String (Result Http.Error (List MaintainerDetail))
    | RequestAccess String
    | RequestCompleted String (Result Http.Error ())
    | DismissMessage


init : String -> AuthState -> ( Model, Cmd Msg )
init apiBaseUrl authState =
    ( { apiBaseUrl = apiBaseUrl
      , authState = authState
      , query = ""
      , results = NotSearched
      , requesting = False
      , error = Nothing
      , success = Nothing
      }
    , Cmd.none
    )


setAuth : AuthState -> Model -> Model
setAuth authState model =
    { model | authState = authState }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        SetQuery query ->
            ( { model | query = query }, Cmd.none )

        Submit ->
            let
                attrPath =
                    String.trim model.query
            in
            if String.isEmpty attrPath then
                ( { model | error = Just "Enter an attribute path to search for." }, Cmd.none )

            else
                ( { model | results = Searching attrPath, error = Nothing, success = Nothing }
                , search model.apiBaseUrl attrPath
                )

        GotMaintainers attrPath (Ok maintainers) ->
            ( { model | results = Found attrPath maintainers }, Cmd.none )

        GotMaintainers attrPath (Err error) ->
            ( { model | results = SearchFailed attrPath error }, Cmd.none )

        RequestAccess attrPath ->
            case Auth.getToken model.authState of
                Just token ->
                    ( { model | requesting = True, error = Nothing, success = Nothing }
                    , requestMaintainerAccess model.apiBaseUrl token attrPath
                    )

                Nothing ->
                    ( { model | error = Just "Sign in to request maintainer access." }, Cmd.none )

        RequestCompleted attrPath (Ok ()) ->
            ( { model | requesting = False, success = Just ("Requested maintainer access to " ++ attrPath ++ ". An admin will review it.") }
            , search model.apiBaseUrl attrPath
            )

        RequestCompleted _ (Err error) ->
            ( { model | requesting = False, error = Just (requestErrorText error) }, Cmd.none )

        DismissMessage ->
            ( { model | error = Nothing, success = Nothing }, Cmd.none )


search : String -> String -> Cmd Msg
search apiBaseUrl attrPath =
    Api.getAttrPathMaintainers apiBaseUrl attrPath (GotMaintainers attrPath)


requestErrorText : Http.Error -> String
requestErrorText error =
    case error of
        Http.BadStatus 409 ->
            "You already have a pending request, or already maintain this."

        _ ->
            "Could not request access. " ++ Ui.httpErrorMessage error



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ Ui.simpleHead "Maintainers" "Look up who maintains an attribute path, and request to maintain it yourself."
        , Ui.messages { error = model.error, success = model.success, onDismiss = DismissMessage }
        , searchForm model
        , viewResults model
        ]


searchForm : Model -> Html Msg
searchForm model =
    form [ class "toolbar", onSubmit Submit, attribute "role" "search" ]
        [ label [ class "field grow" ]
            [ text "Attribute path"
            , input
                [ type_ "search"
                , class "mono"
                , placeholder "e.g. python3Packages.requests"
                , value model.query
                , onInput SetQuery
                , autocomplete False
                , spellcheck False
                ]
                []
            ]
        , button [ type_ "submit", class "button primary", disabled (isSearching model) ]
            [ text
                (if isSearching model then
                    "Searching…"

                 else
                    "Search"
                )
            ]
        ]


isSearching : Model -> Bool
isSearching model =
    case model.results of
        Searching _ ->
            True

        _ ->
            False


viewResults : Model -> Html Msg
viewResults model =
    case model.results of
        NotSearched ->
            Ui.emptyState "Search for an attribute path" "Results show its maintainers and when they were added."

        Searching _ ->
            Ui.loading "Looking up maintainers…"

        SearchFailed attrPath error ->
            Ui.httpError ("Could not look up " ++ attrPath ++ ".") error

        Found attrPath maintainers ->
            Ui.panel ("Maintainers of " ++ attrPath)
                [ requestButton model attrPath maintainers ]
                [ if List.isEmpty maintainers then
                    Ui.emptyNote "Nobody maintains this attribute path yet. Request access to be the first."

                  else
                    Ui.tableWrap (maintainerTable maintainers)
                ]


requestButton : Model -> String -> List MaintainerDetail -> Html Msg
requestButton model attrPath maintainers =
    let
        isMaintainer =
            case model.authState.user of
                Just user ->
                    List.any (\m -> m.githubUserId == user.githubId) maintainers

                Nothing ->
                    False
    in
    if model.authState.loading then
        text ""

    else if not (Auth.isAuthenticated model.authState) then
        a [ class "button small", href Auth.login ] [ text "Sign in to request access" ]

    else if isMaintainer then
        Ui.badge Ui.Good "You maintain this"

    else if model.requesting then
        button [ type_ "button", class "button small", disabled True ] [ text "Requesting…" ]

    else
        button [ type_ "button", class "button small", onClick (RequestAccess attrPath) ] [ text "Request maintainer access" ]


maintainerTable : List MaintainerDetail -> Html Msg
maintainerTable maintainers =
    table [ class "table compact" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Maintainer" ]
                , th [ scope "col" ] [ text "Added" ]
                , th [ scope "col" ] [ text "Added by" ]
                ]
            ]
        , tbody [] (List.map maintainerRow maintainers)
        ]


maintainerRow : MaintainerDetail -> Html Msg
maintainerRow maintainer =
    tr []
        [ td []
            [ span [ class "person" ]
                [ Ui.avatar maintainer.githubAvatarUrl maintainer.githubUsername
                , Ui.externalLink ("https://github.com/" ++ maintainer.githubUsername) [ text maintainer.githubUsername ]
                ]
            ]
        , td [ class "nowrap small" ] [ Ui.timestamp maintainer.addedAt ]
        , td [ class "small" ]
            [ case maintainer.addedByUsername of
                Just name ->
                    text name

                Nothing ->
                    span [ class "muted" ] [ text "Approved request" ]
            ]
        ]



-- API


requestMaintainerAccess : String -> AuthToken -> String -> Cmd Msg
requestMaintainerAccess apiBaseUrl authToken attrPath =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/attr-paths/" ++ attrPath ++ "/request-maintainer"
        , body = Http.emptyBody
        , expect = Http.expectWhatever (RequestCompleted attrPath)
        , timeout = Nothing
        , tracker = Nothing
        }

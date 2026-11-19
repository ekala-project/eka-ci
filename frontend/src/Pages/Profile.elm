module Pages.Profile exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| The signed-in user's profile: account, maintained paths and requests.
-}

import Api.Decoder as Decoder
import Auth exposing (AuthToken)
import Components.Ui as Ui
import Html exposing (Html, a, button, div, h1, li, p, span, table, tbody, td, text, th, thead, tr, ul)
import Html.Attributes exposing (class, href, scope, type_)
import Html.Events exposing (onClick)
import Http
import Json.Decode as D
import Models.Maintainer exposing (MaintainerRequest, RequestStatus(..))
import Route


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


type alias Model =
    { apiBaseUrl : String
    , authToken : AuthToken
    , profile : Remote UserProfile
    , requests : Remote (List MaintainerRequest)
    }


type alias UserProfile =
    { user : UserInfo
    , maintainedPaths : List String
    , createdAt : String
    , lastLogin : String
    }


type alias UserInfo =
    { githubId : Int
    , username : String
    , avatarUrl : Maybe String
    , isAdmin : Bool
    }


type Msg
    = Retry
    | ProfileLoaded (Result Http.Error UserProfile)
    | RequestsLoaded (Result Http.Error (List MaintainerRequest))


init : String -> AuthToken -> ( Model, Cmd Msg )
init apiBaseUrl authToken =
    ( { apiBaseUrl = apiBaseUrl
      , authToken = authToken
      , profile = Loading
      , requests = Loading
      }
    , load apiBaseUrl authToken
    )


load : String -> AuthToken -> Cmd Msg
load apiBaseUrl authToken =
    Cmd.batch
        [ authGet apiBaseUrl authToken "/users/me/profile" (Http.expectJson ProfileLoaded profileDecoder)
        , authGet apiBaseUrl authToken "/users/me/maintainer-requests" (Http.expectJson RequestsLoaded Decoder.maintainerRequestList)
        ]


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        Retry ->
            ( { model | profile = Loading, requests = Loading }, load model.apiBaseUrl model.authToken )

        ProfileLoaded result ->
            ( { model | profile = toRemote result }, Cmd.none )

        RequestsLoaded result ->
            ( { model | requests = toRemote result }, Cmd.none )


toRemote : Result Http.Error a -> Remote a
toRemote result =
    case result of
        Ok value ->
            Ready value

        Err error ->
            Failed error



-- VIEW


view : Model -> Html Msg
view model =
    case model.profile of
        Loading ->
            div [] [ Ui.simpleHead "Profile" "", Ui.loading "Loading your profile…" ]

        Failed error ->
            div []
                [ Ui.simpleHead "Profile" ""
                , Ui.httpError "Could not load your profile." error
                , button [ type_ "button", class "button", onClick Retry ] [ text "Try again" ]
                ]

        Ready profile ->
            div []
                [ viewHead profile
                , div [ class "grid-2" ]
                    [ div [ class "stack" ]
                        [ viewAccount profile
                        , viewMaintained profile.maintainedPaths
                        ]
                    , viewRequests model.requests
                    ]
                ]


viewHead : UserProfile -> Html Msg
viewHead profile =
    div [ class "profile-head" ]
        [ Ui.avatarLarge profile.user.avatarUrl profile.user.username
        , div []
            [ div [ class "title-badge" ]
                [ h1 [] [ text profile.user.username ]
                , if profile.user.isAdmin then
                    Ui.badge Ui.Info "Admin"

                  else
                    text ""
                ]
            , p [ class "muted small" ]
                [ Ui.externalLink ("https://github.com/" ++ profile.user.username) [ text "View on GitHub" ] ]
            ]
        ]


viewAccount : UserProfile -> Html Msg
viewAccount profile =
    Ui.panel "Account"
        []
        [ Ui.facts
            [ ( "GitHub ID", span [ class "mono" ] [ text (String.fromInt profile.user.githubId) ] )
            , ( "Member since", Ui.timestamp profile.createdAt )
            , ( "Last sign-in", Ui.timestamp profile.lastLogin )
            , ( "Role"
              , text
                    (if profile.user.isAdmin then
                        "Admin"

                     else
                        "Member"
                    )
              )
            ]
        ]


viewMaintained : List String -> Html Msg
viewMaintained paths =
    Ui.panel "Maintained attribute paths"
        [ a [ class "button small", href (Route.toHref Route.AttrPathSearch) ] [ text "Find more" ] ]
        [ if List.isEmpty paths then
            Ui.emptyNote "You don't maintain any attribute paths yet."

          else
            ul [ class "list-plain" ] (List.map (\path -> li [ class "mono" ] [ text path ]) (List.sort paths))
        ]


viewRequests : Remote (List MaintainerRequest) -> Html Msg
viewRequests remote =
    Ui.panel "Maintainer requests"
        []
        [ case remote of
            Loading ->
                Ui.loadingInline "Loading requests…"

            Failed error ->
                Ui.httpError "Could not load your requests." error

            Ready [] ->
                Ui.emptyNote "You haven't requested to maintain anything."

            Ready requests ->
                Ui.tableWrap (requestTable requests)
        ]


requestTable : List MaintainerRequest -> Html Msg
requestTable requests =
    table [ class "table compact" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Attribute path" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col", class "hide-sm" ] [ text "Requested" ]
                ]
            ]
        , tbody [] (List.map requestRow requests)
        ]


requestRow : MaintainerRequest -> Html Msg
requestRow request =
    tr []
        [ td [ class "mono" ] [ text request.attrPath ]
        , td [] [ statusBadge request, reviewNote request ]
        , td [ class "nowrap small hide-sm" ] [ Ui.timestamp request.requestedAt ]
        ]


statusBadge : MaintainerRequest -> Html msg
statusBadge request =
    case request.status of
        Pending ->
            Ui.badge Ui.Warn "Pending"

        Approved ->
            Ui.badge Ui.Good "Approved"

        Rejected ->
            Ui.badge Ui.Critical "Rejected"


reviewNote : MaintainerRequest -> Html msg
reviewNote request =
    case ( request.reviewedByUsername, request.reviewedAt ) of
        ( Just reviewer, Just reviewedAt ) ->
            span [ class "cell-sub" ] [ text ("by " ++ reviewer ++ ", "), Ui.timestamp reviewedAt ]

        _ ->
            text ""



-- API


authGet : String -> AuthToken -> String -> Http.Expect Msg -> Cmd Msg
authGet apiBaseUrl authToken path expect =
    Http.request
        { method = "GET"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ path
        , body = Http.emptyBody
        , expect = expect
        , timeout = Nothing
        , tracker = Nothing
        }


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

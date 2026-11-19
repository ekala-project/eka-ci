module Pages.Job exposing
    ( Model
    , Msg(..)
    , init
    , setAuth
    , update
    , view
    )

{-| Job page: a jobset's progress, its maintainers, and every derivation.

Derivations can be filtered by status bucket and by name, and are listed
problems first. Build state changes arrive over the WebSocket.

-}

import Api.Api as Api
import Auth exposing (AuthState)
import Components.BuildProgress as Progress exposing (Bucket)
import Components.StatusBadge as StatusBadge
import Components.Ui as Ui
import Html exposing (Html, a, button, div, h1, input, label, li, p, span, table, tbody, td, text, th, thead, tr, ul)
import Html.Attributes exposing (attribute, class, disabled, href, placeholder, scope, title, type_, value)
import Html.Events exposing (onClick, onInput)
import Html.Lazy exposing (lazy4)
import Http
import Models.BuildState as BS
import Models.Job exposing (JobSetDetails, JobSetDrv)
import Models.Maintainer exposing (MaintainerDetail)
import Ports
import Route


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


type alias Model =
    { apiBaseUrl : String
    , jobsetId : Int
    , authState : AuthState
    , details : Remote JobSetDetails
    , drvs : Remote (List JobSetDrv)
    , maintainers : Remote (List MaintainerDetail)
    , requestingAccess : Bool
    , successMessage : Maybe String
    , errorMessage : Maybe String
    , filter : Maybe Bucket
    , search : String
    , limit : Int
    }


type Msg
    = GotJobSetDetails (Result Http.Error JobSetDetails)
    | GotJobSetDrvs (Result Http.Error (List JobSetDrv))
    | GotMaintainers (Result Http.Error (List MaintainerDetail))
    | RequestMaintainerAccess
    | MaintainerRequestCompleted (Result Http.Error ())
    | DismissMessage
    | SetFilter (Maybe Bucket)
    | SetSearch String
    | ShowMore
    | BuildStateChanged Ports.BuildStateChangeEvent
    | JobCompleted Ports.JobCompleteEvent


pageSize : Int
pageSize =
    200


init : String -> Int -> AuthState -> ( Model, Cmd Msg )
init apiBaseUrl jobsetId authState =
    ( { apiBaseUrl = apiBaseUrl
      , jobsetId = jobsetId
      , authState = authState
      , details = Loading
      , drvs = Loading
      , maintainers = Loading
      , requestingAccess = False
      , successMessage = Nothing
      , errorMessage = Nothing
      , filter = Nothing
      , search = ""
      , limit = pageSize
      }
    , Cmd.batch
        [ Api.getJobSetDetails apiBaseUrl jobsetId GotJobSetDetails
        , Api.getJobSetDrvs apiBaseUrl jobsetId GotJobSetDrvs
        , Api.getJobMaintainers apiBaseUrl jobsetId GotMaintainers
        , Ports.websocketOut (Ports.encodeSubscribeMessage "job" (String.fromInt jobsetId))
        ]
    )


{-| Keep the page's view of the session in sync (e.g. after a reload
resolves the stored token).
-}
setAuth : AuthState -> Model -> Model
setAuth authState model =
    { model | authState = authState }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotJobSetDetails result ->
            ( { model | details = toRemote result }, Cmd.none )

        GotJobSetDrvs result ->
            ( { model | drvs = toRemote result }, Cmd.none )

        GotMaintainers result ->
            ( { model | maintainers = toRemote result }, Cmd.none )

        RequestMaintainerAccess ->
            requestAccess model

        MaintainerRequestCompleted (Ok _) ->
            ( { model
                | requestingAccess = False
                , successMessage = Just "Maintainer access requested. An admin will review it."
                , errorMessage = Nothing
              }
            , Api.getJobMaintainers model.apiBaseUrl model.jobsetId GotMaintainers
            )

        MaintainerRequestCompleted (Err error) ->
            ( { model | requestingAccess = False, errorMessage = Just (requestErrorText error) }, Cmd.none )

        DismissMessage ->
            ( { model | successMessage = Nothing, errorMessage = Nothing }, Cmd.none )

        SetFilter filter ->
            ( { model | filter = filter, limit = pageSize }, Cmd.none )

        SetSearch search ->
            ( { model | search = search, limit = pageSize }, Cmd.none )

        ShowMore ->
            ( { model | limit = model.limit + pageSize }, Cmd.none )

        BuildStateChanged event ->
            ( { model | drvs = mapRemote (updateDrvState event.drvPath event.newState) model.drvs }, Cmd.none )

        JobCompleted event ->
            if event.jobsetId == model.jobsetId then
                ( model
                , Cmd.batch
                    [ Api.getJobSetDetails model.apiBaseUrl model.jobsetId GotJobSetDetails
                    , Api.getJobSetDrvs model.apiBaseUrl model.jobsetId GotJobSetDrvs
                    ]
                )

            else
                ( model, Cmd.none )


requestAccess : Model -> ( Model, Cmd Msg )
requestAccess model =
    case ( Auth.getToken model.authState, model.details ) of
        ( Just token, Ready details ) ->
            ( { model | requestingAccess = True, errorMessage = Nothing, successMessage = Nothing }
            , requestMaintainerAccess model.apiBaseUrl token details.jobName
            )

        ( Nothing, _ ) ->
            ( { model | errorMessage = Just "Sign in to request maintainer access." }, Cmd.none )

        _ ->
            ( model, Cmd.none )


toRemote : Result Http.Error a -> Remote a
toRemote result =
    case result of
        Ok value ->
            Ready value

        Err error ->
            Failed error


mapRemote : (a -> a) -> Remote a -> Remote a
mapRemote f remote =
    case remote of
        Ready value ->
            Ready (f value)

        _ ->
            remote


updateDrvState : String -> BS.DrvBuildState -> List JobSetDrv -> List JobSetDrv
updateDrvState drvPath newState =
    List.map
        (\drv ->
            if drv.drvPath == drvPath then
                { drv | buildState = newState }

            else
                drv
        )


{-| Progress from the live per-derivation states once they have loaded, so
the bar, legend and filter chips always agree; the jobset summary until then.
-}
counts : Model -> Maybe Progress.Counts
counts model =
    case ( model.drvs, model.details ) of
        ( Ready drvs, _ ) ->
            Just (Progress.fromStates (List.map .buildState drvs))

        ( _, Ready details ) ->
            Just (Progress.fromJobSet details)

        _ ->
            Nothing



-- VIEW


view : Model -> Html Msg
view model =
    case model.details of
        Loading ->
            div []
                [ Ui.simpleHead ("Job #" ++ String.fromInt model.jobsetId) ""
                , Ui.loading "Loading job…"
                ]

        Failed error ->
            div []
                [ Ui.simpleHead ("Job #" ++ String.fromInt model.jobsetId) ""
                , Ui.httpError "Could not load this job." error
                ]

        Ready details ->
            div []
                [ viewHead model details
                , Ui.messages { error = model.errorMessage, success = model.successMessage, onDismiss = DismissMessage }
                , div [ class "grid-2" ]
                    [ viewProgress model
                    , viewMaintainers model
                    ]
                , viewDerivations model
                ]


viewHead : Model -> JobSetDetails -> Html Msg
viewHead model details =
    Ui.pageHead
        { crumbs =
            [ ( "Repositories", Route.toHref Route.Home )
            , ( details.owner ++ "/" ++ details.repoName, Route.toHref (Route.Repository details.owner details.repoName) )
            , ( "Commit " ++ Ui.shortSha details.sha, Route.toHref (Route.Commit details.sha) )
            ]
        , title =
            [ h1 [] [ text details.jobName ]
            , counts model |> Maybe.map Progress.overallBadge |> Maybe.withDefault (text "")
            ]
        , lede =
            [ text ("Job #" ++ String.fromInt details.jobsetId ++ " at commit ")
            , a [ class "mono", href (Route.toHref (Route.Commit details.sha)), title details.sha ] [ text (Ui.shortSha details.sha) ]
            ]
        , actions = []
        }


viewProgress : Model -> Html Msg
viewProgress model =
    Ui.panel "Progress"
        []
        [ case counts model of
            Just c ->
                Progress.view c

            Nothing ->
                Ui.loadingInline "Loading progress…"
        ]



-- MAINTAINERS


viewMaintainers : Model -> Html Msg
viewMaintainers model =
    Ui.panel "Maintainers"
        [ requestButton model ]
        [ case model.maintainers of
            Loading ->
                Ui.loadingInline "Loading maintainers…"

            Failed _ ->
                p [ class "muted" ] [ text "Could not load maintainers." ]

            Ready [] ->
                p [ class "muted" ] [ text "Nobody maintains this job yet." ]

            Ready maintainers ->
                ul [ class "people" ] (List.map viewMaintainer maintainers)
        ]


viewMaintainer : MaintainerDetail -> Html Msg
viewMaintainer maintainer =
    li [ class "person", title ("Maintainer since " ++ maintainer.addedAt) ]
        [ Ui.avatar maintainer.githubAvatarUrl maintainer.githubUsername
        , text maintainer.githubUsername
        ]


requestButton : Model -> Html Msg
requestButton model =
    let
        isMaintainer =
            case ( model.authState.user, model.maintainers ) of
                ( Just user, Ready maintainers ) ->
                    List.any (\m -> m.githubUserId == user.githubId) maintainers

                _ ->
                    False
    in
    if model.authState.loading then
        text ""

    else if not (Auth.isAuthenticated model.authState) then
        a [ class "button small", href Auth.login ] [ text "Sign in to request access" ]

    else if isMaintainer then
        Ui.badge Ui.Good "You maintain this"

    else if model.requestingAccess then
        button [ type_ "button", class "button small", disabled True ] [ text "Requesting…" ]

    else
        button [ type_ "button", class "button small", onClick RequestMaintainerAccess ] [ text "Request maintainer access" ]



-- DERIVATIONS


viewDerivations : Model -> Html Msg
viewDerivations model =
    case model.drvs of
        Loading ->
            Ui.panel "Derivations" [] [ Ui.loadingInline "Loading derivations…" ]

        Failed error ->
            Ui.panel "Derivations" [] [ Ui.httpError "Could not load derivations." error ]

        Ready [] ->
            Ui.panel "Derivations" [] [ Ui.emptyNote "This job has no derivations." ]

        Ready drvs ->
            lazy4 viewDerivationPanel model.filter model.search model.limit drvs


viewDerivationPanel : Maybe Bucket -> String -> Int -> List JobSetDrv -> Html Msg
viewDerivationPanel filter search limit drvs =
    let
        matching =
            drvs
                |> List.filter (matchesFilter filter search)
                |> List.sortBy (\drv -> ( Progress.bucketRank (Progress.bucketOf drv.buildState), drv.name ))

        shown =
            List.take limit matching

        matchCount =
            List.length matching
    in
    Ui.panel "Derivations"
        [ span [ class "small muted", attribute "aria-live" "polite" ] [ text (shownText (List.length shown) matchCount (List.length drvs)) ] ]
        [ filterChips filter drvs
        , div [ class "toolbar" ]
            [ label [ class "field grow" ]
                [ text "Filter by name"
                , input [ type_ "search", placeholder "e.g. python3", value search, onInput SetSearch ] []
                ]
            ]
        , if List.isEmpty shown then
            Ui.emptyNote "No derivations match these filters."

          else
            Ui.tableWrap (drvTable shown)
        , if matchCount > limit then
            div [ class "more" ]
                [ button [ type_ "button", class "button", onClick ShowMore ]
                    [ text ("Show " ++ String.fromInt (min pageSize (matchCount - limit)) ++ " more") ]
                ]

          else
            text ""
        ]


shownText : Int -> Int -> Int -> String
shownText shown matching total =
    if shown == total then
        String.fromInt total ++ " derivations"

    else if shown == matching then
        String.fromInt matching ++ " of " ++ String.fromInt total ++ " derivations"

    else
        "Showing " ++ String.fromInt shown ++ " of " ++ String.fromInt matching ++ " matching"


matchesFilter : Maybe Bucket -> String -> JobSetDrv -> Bool
matchesFilter filter search drv =
    let
        needle =
            String.toLower (String.trim search)

        bucketOk =
            case filter of
                Just bucket ->
                    Progress.bucketOf drv.buildState == bucket

                Nothing ->
                    True
    in
    bucketOk
        && (String.isEmpty needle
                || String.contains needle (String.toLower drv.name)
                || String.contains needle (String.toLower drv.drvPath)
           )


filterChips : Maybe Bucket -> List JobSetDrv -> Html Msg
filterChips filter drvs =
    let
        c =
            Progress.fromStates (List.map .buildState drvs)

        bucketChip bucket =
            Ui.chip
                { label = Progress.bucketLabel bucket
                , count = Just (Progress.countOf bucket c)
                , tone = Just (Progress.bucketTone bucket)
                , pressed = filter == Just bucket
                , onPress =
                    if filter == Just bucket then
                        SetFilter Nothing

                    else
                        SetFilter (Just bucket)
                }
    in
    div [ class "chips", attribute "role" "group", attribute "aria-label" "Filter by status" ]
        (Ui.chip { label = "All", count = Just c.total, tone = Nothing, pressed = filter == Nothing, onPress = SetFilter Nothing }
            :: (Progress.filterOrder
                    |> List.filter (\b -> Progress.countOf b c > 0 || filter == Just b)
                    |> List.map bucketChip
               )
        )


drvTable : List JobSetDrv -> Html Msg
drvTable drvs =
    table [ class "table compact" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Name" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col" ] [ text "System" ]
                , th [ scope "col", class "hide-sm" ] [ text "Type" ]
                ]
            ]
        , tbody [] (List.map drvRow drvs)
        ]


drvRow : JobSetDrv -> Html Msg
drvRow drv =
    tr []
        [ td []
            [ a [ class "cell-main", href (Route.toHref (Route.Drv drv.drvPath)), title ("/nix/store/" ++ drv.drvPath) ]
                [ text drv.name ]
            ]
        , td [] [ StatusBadge.view drv.buildState ]
        , td [ class "mono small nowrap" ] [ text drv.system ]
        , td [ class "hide-sm" ] [ drvTags drv ]
        ]


drvTags : { a | isFod : Bool, preferLocalBuild : Bool } -> Html msg
drvTags drv =
    let
        tags =
            List.filterMap identity
                [ if drv.isFod then
                    Just (span [ class "tag", title "Fixed-output derivation (fetches sources)" ] [ text "FOD" ])

                  else
                    Nothing
                , if drv.preferLocalBuild then
                    Just (span [ class "tag", title "Prefers a local build" ] [ text "Local" ])

                  else
                    Nothing
                ]
    in
    span [ class "tags" ] tags



-- HTTP


requestMaintainerAccess : String -> String -> String -> Cmd Msg
requestMaintainerAccess apiBaseUrl authToken attrPath =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/attr-paths/" ++ attrPath ++ "/request-maintainer"
        , body = Http.emptyBody
        , expect = Http.expectWhatever MaintainerRequestCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


requestErrorText : Http.Error -> String
requestErrorText error =
    case error of
        Http.BadStatus 409 ->
            "You already have a pending request, or already maintain this."

        _ ->
            "Could not request access. " ++ Ui.httpErrorMessage error

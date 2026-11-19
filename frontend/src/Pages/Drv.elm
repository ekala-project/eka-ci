module Pages.Drv exposing
    ( Model
    , Msg(..)
    , init
    , update
    , view
    )

{-| Derivation page: status, metadata, direct dependencies and the build log.

The stored log is fetched on load; new lines stream in over the WebSocket.

-}

import Api.Api as Api
import Components.BuildProgress as Progress
import Components.LogViewer as LogViewer
import Components.StatusBadge as StatusBadge
import Components.Ui as Ui
import Html exposing (Html, a, div, h1, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, href, scope, title)
import Http
import Models.BuildState as BS
import Models.Derivation exposing (DrvDependency, DrvDetails)
import Ports
import Route


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


type alias Model =
    { apiBaseUrl : String
    , drvPath : String
    , details : Remote DrvDetails
    , dependencies : Remote (List DrvDependency)
    , log : LogViewer.Model
    }


type Msg
    = GotDrvDetails (Result Http.Error DrvDetails)
    | GotDrvDependencies (Result Http.Error (List DrvDependency))
    | GotLog (Result Http.Error String)
    | LogViewerMsg LogViewer.Msg
    | BuildStateChanged Ports.BuildStateChangeEvent
    | LogLineReceived Ports.LogLineEvent


init : String -> String -> ( Model, Cmd Msg )
init apiBaseUrl drvPath =
    ( { apiBaseUrl = apiBaseUrl
      , drvPath = drvPath
      , details = Loading
      , dependencies = Loading
      , log = LogViewer.fetching LogViewer.init
      }
    , Cmd.batch
        [ Api.getDrvDetails apiBaseUrl drvPath GotDrvDetails
        , Api.getDrvDependencies apiBaseUrl drvPath GotDrvDependencies
        , Api.getDrvLog apiBaseUrl drvPath GotLog
        , Ports.websocketOut (Ports.encodeSubscribeMessage "drv" drvPath)
        ]
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotDrvDetails result ->
            ( { model | details = toRemote result }, Cmd.none )

        GotDrvDependencies result ->
            ( { model | dependencies = toRemote result }, Cmd.none )

        GotLog result ->
            withLog (LogViewer.setStored result model.log) model

        LogViewerMsg logMsg ->
            let
                ( log, cmd ) =
                    LogViewer.update logMsg model.log
            in
            ( { model | log = log }, Cmd.map LogViewerMsg cmd )

        BuildStateChanged event ->
            if event.drvPath == model.drvPath then
                buildStateChanged event model

            else
                ( model, Cmd.none )

        LogLineReceived event ->
            if event.drvPath == model.drvPath then
                withLog (LogViewer.addLine event.line model.log) model

            else
                ( model, Cmd.none )


withLog : LogViewer.Model -> Model -> ( Model, Cmd Msg )
withLog log model =
    ( { model | log = log }, Cmd.map LogViewerMsg (LogViewer.scrollToEnd log) )


{-| Update the status; when the build finishes, re-read the stored log so
the viewer shows the complete output even if streamed lines were missed.
-}
buildStateChanged : Ports.BuildStateChangeEvent -> Model -> ( Model, Cmd Msg )
buildStateChanged event model =
    let
        details =
            case model.details of
                Ready d ->
                    Ready { d | buildState = event.newState }

                other ->
                    other

        refetch =
            if BS.isTerminal event.newState && not (BS.isTerminal event.oldState) then
                Api.getDrvLog model.apiBaseUrl model.drvPath GotLog

            else
                Cmd.none
    in
    ( { model | details = details }, refetch )


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
    case model.details of
        Loading ->
            div []
                [ viewHead model Nothing
                , Ui.loading "Loading derivation…"
                ]

        Failed error ->
            div []
                [ viewHead model Nothing
                , Ui.httpError "Could not load this derivation." error
                ]

        Ready details ->
            div []
                [ viewHead model (Just details)
                , div [ class "grid-2" ]
                    [ viewDetails details
                    , viewDependencies model.dependencies
                    ]
                , Ui.panel "Build log"
                    []
                    [ Html.map LogViewerMsg (LogViewer.view (model.apiBaseUrl ++ "/logs/" ++ model.drvPath) model.log) ]
                ]


viewHead : Model -> Maybe DrvDetails -> Html Msg
viewHead model maybeDetails =
    Ui.pageHead
        { crumbs = [ ( "Repositories", Route.toHref Route.Home ) ]
        , title =
            case maybeDetails of
                Just details ->
                    [ h1 [] [ text details.name ], StatusBadge.view details.buildState ]

                Nothing ->
                    [ h1 [] [ text "Derivation" ] ]
        , lede = [ span [ class "path" ] [ text (storePath model.drvPath) ] ]
        , actions = []
        }


storePath : String -> String
storePath drvPath =
    if String.startsWith "/nix/store/" drvPath then
        drvPath

    else
        "/nix/store/" ++ drvPath


viewDetails : DrvDetails -> Html Msg
viewDetails details =
    Ui.panel "Details"
        []
        [ Ui.facts
            [ ( "Status", StatusBadge.view details.buildState )
            , ( "System", span [ class "mono" ] [ text details.system ] )
            , ( "Fixed-output", text (yesNo details.isFod) )
            , ( "Prefers local build", text (yesNo details.preferLocalBuild) )
            , ( "Derivation", span [ class "path" ] [ text (storePath details.drvPath) ] )
            ]
        ]


yesNo : Bool -> String
yesNo value =
    if value then
        "Yes"

    else
        "No"


viewDependencies : Remote (List DrvDependency) -> Html Msg
viewDependencies remote =
    case remote of
        Loading ->
            Ui.panel "Dependencies" [] [ Ui.loadingInline "Loading dependencies…" ]

        Failed error ->
            Ui.panel "Dependencies" [] [ Ui.httpError "Could not load dependencies." error ]

        Ready [] ->
            Ui.panel "Dependencies" [] [ Ui.emptyNote "No dependencies that EkaCI builds." ]

        Ready deps ->
            Ui.panel "Dependencies"
                [ span [ class "small muted" ] [ text (String.fromInt (List.length deps) ++ " direct") ] ]
                [ Ui.tableWrap (dependencyTable deps) ]


dependencyTable : List DrvDependency -> Html Msg
dependencyTable deps =
    table [ class "table compact" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Name" ]
                , th [ scope "col" ] [ text "Status" ]
                ]
            ]
        , tbody []
            (deps
                |> List.sortBy (\dep -> ( Progress.bucketRank (Progress.bucketOf dep.buildState), dep.name ))
                |> List.map dependencyRow
            )
        ]


dependencyRow : DrvDependency -> Html Msg
dependencyRow dep =
    tr []
        [ td []
            [ a [ class "cell-main", href (Route.toHref (Route.Drv dep.drvPath)), title (storePath dep.drvPath) ]
                [ text dep.name ]
            ]
        , td [] [ StatusBadge.view dep.buildState ]
        ]

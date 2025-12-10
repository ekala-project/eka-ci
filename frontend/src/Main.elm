module Main exposing (main)

import Browser
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Http
import Json.Decode as D
import Url
import Url.Parser as Parser exposing ((</>), Parser, string)


main : Program () Model Msg
main =
    Browser.application
        { init = init
        , view = view
        , update = update
        , subscriptions = subscriptions
        , onUrlChange = UrlChanged
        , onUrlRequest = LinkClicked
        }



-- ROUTE


type Route
    = Home
    | CommitDetail String
    | NotFound


routeParser : Parser (Route -> a) a
routeParser =
    Parser.oneOf
        [ Parser.map Home Parser.top
        , Parser.map CommitDetail (Parser.s "commits" </> string)
        ]


fromUrl : Url.Url -> Route
fromUrl url =
    Parser.parse routeParser url
        |> Maybe.withDefault NotFound



-- MODEL


type alias CheckRun =
    { checkRunId : Int
    , repoName : String
    , repoOwner : String
    , buildState : DrvBuildState
    , drvPath : String
    }


type DrvBuildState
    = Queued
    | Buildable
    | Building
    | Completed DrvBuildResult
    | Interrupted DrvBuildInterruptionKind
    | TransitiveFailure
    | Blocked


type DrvBuildResult
    = Success
    | Failure


type DrvBuildInterruptionKind
    = OutOfMemory
    | Timeout
    | Cancelled
    | ProcessDeath
    | SchedulerDeath


type LoadingState
    = Loading
    | Loaded (List CheckRun)
    | LoadError String


type alias Model =
    { navKey : Nav.Key
    , currentUrl : Url.Url
    , route : Route
    , checkRuns : LoadingState
    }


init : () -> Url.Url -> Nav.Key -> ( Model, Cmd Msg )
init flags url key =
    let
        route =
            fromUrl url

        model =
            { navKey = key
            , currentUrl = url
            , route = route
            , checkRuns = Loading
            }
    in
    case route of
        CommitDetail sha ->
            ( model, fetchCheckRuns sha )

        _ ->
            ( { model | checkRuns = Loaded [] }, Cmd.none )



-- UPDATE


type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url.Url
    | GotCheckRuns (Result Http.Error (List CheckRun))


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        LinkClicked urlRequest ->
            case urlRequest of
                Browser.Internal url ->
                    ( model, Nav.pushUrl model.navKey (Url.toString url) )

                Browser.External href ->
                    ( model, Nav.load href )

        UrlChanged url ->
            let
                route =
                    fromUrl url

                newModel =
                    { model | currentUrl = url, route = route, checkRuns = Loading }
            in
            case route of
                CommitDetail sha ->
                    ( newModel, fetchCheckRuns sha )

                _ ->
                    ( { newModel | checkRuns = Loaded [] }, Cmd.none )

        GotCheckRuns result ->
            case result of
                Ok checkRuns ->
                    ( { model | checkRuns = Loaded checkRuns }, Cmd.none )

                Err error ->
                    ( { model | checkRuns = LoadError (httpErrorToString error) }, Cmd.none )



-- HTTP


fetchCheckRuns : String -> Cmd Msg
fetchCheckRuns sha =
    Http.get
        { url = "/v1/commits/" ++ sha ++ "/check_runs"
        , expect = Http.expectJson GotCheckRuns checkRunsDecoder
        }


checkRunsDecoder : D.Decoder (List CheckRun)
checkRunsDecoder =
    D.list checkRunDecoder


checkRunDecoder : D.Decoder CheckRun
checkRunDecoder =
    D.map5 CheckRun
        (D.field "check_run_id" D.int)
        (D.field "repo_name" D.string)
        (D.field "repo_owner" D.string)
        (D.field "build_state" buildStateDecoder)
        (D.field "drv_path" D.string)


buildStateDecoder : D.Decoder DrvBuildState
buildStateDecoder =
    D.oneOf
        [ D.string
            |> D.andThen
                (\tag ->
                    case tag of
                        "Queued" ->
                            D.succeed Queued

                        "Buildable" ->
                            D.succeed Buildable

                        "Building" ->
                            D.succeed Building

                        "TransitiveFailure" ->
                            D.succeed TransitiveFailure

                        "Blocked" ->
                            D.succeed Blocked

                        _ ->
                            D.fail ("Unknown build state: " ++ tag)
                )
        , D.field "Completed" buildResultDecoder
            |> D.map Completed
        , D.field "Interrupted" interruptionKindDecoder
            |> D.map Interrupted
        ]


buildResultDecoder : D.Decoder DrvBuildResult
buildResultDecoder =
    D.string
        |> D.andThen
            (\tag ->
                case tag of
                    "Success" ->
                        D.succeed Success

                    "Failure" ->
                        D.succeed Failure

                    _ ->
                        D.fail ("Unknown build result: " ++ tag)
            )


interruptionKindDecoder : D.Decoder DrvBuildInterruptionKind
interruptionKindDecoder =
    D.string
        |> D.andThen
            (\tag ->
                case tag of
                    "OutOfMemory" ->
                        D.succeed OutOfMemory

                    "Timeout" ->
                        D.succeed Timeout

                    "Cancelled" ->
                        D.succeed Cancelled

                    "ProcessDeath" ->
                        D.succeed ProcessDeath

                    "SchedulerDeath" ->
                        D.succeed SchedulerDeath

                    _ ->
                        D.fail ("Unknown interruption kind: " ++ tag)
            )


httpErrorToString : Http.Error -> String
httpErrorToString error =
    case error of
        Http.BadUrl url ->
            "Invalid URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus status ->
            "Request failed with status: " ++ String.fromInt status

        Http.BadBody body ->
            "Failed to decode response: " ++ body



-- SUBSCRIPTIONS


subscriptions : Model -> Sub Msg
subscriptions _ =
    Sub.none



-- VIEW


view : Model -> Browser.Document Msg
view model =
    { title = "eka-ci"
    , body =
        [ div [ style "padding" "20px", style "font-family" "sans-serif" ]
            [ case model.route of
                Home ->
                    viewHome

                CommitDetail sha ->
                    viewCommitDetail sha model.checkRuns

                NotFound ->
                    viewNotFound
            ]
        ]
    }


viewHome : Html Msg
viewHome =
    div []
        [ h1 [] [ text "Eka CI" ]
        , p [] [ text "Enter a commit SHA in the URL to view check runs:" ]
        , p [ style "font-family" "monospace" ] [ text "/commits/{sha}" ]
        ]


viewCommitDetail : String -> LoadingState -> Html Msg
viewCommitDetail sha loadingState =
    div []
        [ h1 [] [ text "Commit Check Runs" ]
        , p [ style "font-family" "monospace", style "color" "#666" ] [ text ("SHA: " ++ sha) ]
        , case loadingState of
            Loading ->
                p [] [ text "Loading check runs..." ]

            LoadError error ->
                div [ style "color" "red" ]
                    [ p [] [ text "Error loading check runs:" ]
                    , p [ style "font-family" "monospace" ] [ text error ]
                    ]

            Loaded checkRuns ->
                if List.isEmpty checkRuns then
                    p [] [ text "No check runs found for this commit." ]

                else
                    viewCheckRuns checkRuns
        ]


viewCheckRuns : List CheckRun -> Html Msg
viewCheckRuns checkRuns =
    div []
        [ h2 [] [ text ("Check Runs (" ++ String.fromInt (List.length checkRuns) ++ ")") ]
        , table [ style "border-collapse" "collapse", style "width" "100%" ]
            [ thead []
                [ tr []
                    [ th [ style "text-align" "left", style "padding" "8px", style "border-bottom" "2px solid #ddd" ] [ text "Build State" ]
                    , th [ style "text-align" "left", style "padding" "8px", style "border-bottom" "2px solid #ddd" ] [ text "Derivation Path" ]
                    , th [ style "text-align" "left", style "padding" "8px", style "border-bottom" "2px solid #ddd" ] [ text "Repository" ]
                    ]
                ]
            , tbody [] (List.map viewCheckRun checkRuns)
            ]
        ]


viewCheckRun : CheckRun -> Html Msg
viewCheckRun checkRun =
    tr [ style "border-bottom" "1px solid #eee" ]
        [ td [ style "padding" "8px" ]
            [ viewBuildState checkRun.buildState ]
        , td [ style "padding" "8px", style "font-family" "monospace", style "font-size" "12px" ]
            [ text checkRun.drvPath ]
        , td [ style "padding" "8px" ]
            [ text (checkRun.repoOwner ++ "/" ++ checkRun.repoName) ]
        ]


viewBuildState : DrvBuildState -> Html Msg
viewBuildState state =
    let
        ( color, label ) =
            case state of
                Queued ->
                    ( "#808080", "Queued" )

                Buildable ->
                    ( "#808080", "Buildable" )

                Building ->
                    ( "#FFA500", "Building" )

                Completed Success ->
                    ( "#28A745", "Success" )

                Completed Failure ->
                    ( "#DC3545", "Failure" )

                Interrupted OutOfMemory ->
                    ( "#DC3545", "OOM" )

                Interrupted Timeout ->
                    ( "#FFC107", "Timeout" )

                Interrupted Cancelled ->
                    ( "#6C757D", "Cancelled" )

                Interrupted ProcessDeath ->
                    ( "#DC3545", "Process Death" )

                Interrupted SchedulerDeath ->
                    ( "#DC3545", "Scheduler Death" )

                TransitiveFailure ->
                    ( "#DC3545", "Transitive Failure" )

                Blocked ->
                    ( "#6C757D", "Blocked" )
    in
    span
        [ style "background-color" color
        , style "color" "white"
        , style "padding" "4px 8px"
        , style "border-radius" "4px"
        , style "font-weight" "bold"
        , style "font-size" "12px"
        , style "display" "inline-block"
        ]
        [ text label ]


viewNotFound : Html Msg
viewNotFound =
    div []
        [ h1 [] [ text "404 - Not Found" ]
        , p [] [ text "The page you're looking for doesn't exist." ]
        ]

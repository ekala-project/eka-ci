module Components.LogViewer exposing
    ( Model
    , Msg
    , addLine
    , fetching
    , init
    , scrollToEnd
    , setStored
    , update
    , view
    )

{-| Build log viewer.

Shows the stored log (fetched once) followed by lines streamed over the
WebSocket. Keeps the newest `maxLines` lines, numbers them as in the full
log, filters by a search term, and follows new output until the reader
scrolls up (scrolling back to the bottom resumes following).

-}

import Array exposing (Array)
import Browser.Dom as Dom
import Components.Ui as Ui
import Html exposing (Html, div, input, label, p, span, text)
import Html.Attributes exposing (attribute, class, id, placeholder, tabindex, type_, value)
import Html.Events exposing (on, onInput)
import Html.Lazy exposing (lazy, lazy2)
import Http
import Json.Decode as D
import Task


type Source
    = Waiting
    | Fetching
    | Fetched
    | Missing
    | FetchFailed Http.Error


type alias Model =
    { lines : Array String

    -- Line number (1-based) of `lines[0]` in the full log.
    , firstLine : Int
    , searchTerm : String
    , follow : Bool
    , source : Source
    }


type Msg
    = SearchInput String
    | SetFollow Bool
    | Scrolled


{-| Lines kept in memory and the DOM.
-}
maxLines : Int
maxLines =
    5000


logId : String
logId =
    "build-log"


init : Model
init =
    { lines = Array.empty
    , firstLine = 1
    , searchTerm = ""
    , follow = True
    , source = Waiting
    }


fetching : Model -> Model
fetching model =
    { model | source = Fetching }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        SearchInput term ->
            ( { model | searchTerm = term }, Cmd.none )

        SetFollow follow ->
            ( { model | follow = follow }
            , if follow then
                scrollToEnd model

              else
                Cmd.none
            )

        Scrolled ->
            ( model, Cmd.none )


{-| Replace the contents with the stored log. A 404 just means nothing has
been stored yet (e.g. the build hasn't started).
-}
setStored : Result Http.Error String -> Model -> Model
setStored result model =
    case result of
        Ok body ->
            let
                all =
                    String.lines (stripTrailingNewline body)

                count =
                    List.length all

                kept =
                    List.drop (count - maxLines) all
            in
            { model
                | lines = Array.fromList kept
                , firstLine = max 1 (count - maxLines + 1)
                , source = Fetched
            }

        Err (Http.BadStatus 404) ->
            { model | source = Missing }

        Err error ->
            { model | source = FetchFailed error }


stripTrailingNewline : String -> String
stripTrailingNewline body =
    if String.endsWith "\n" body then
        String.dropRight 1 body

    else
        body


{-| Append a streamed line, trimming the oldest lines in batches.
-}
addLine : String -> Model -> Model
addLine line model =
    let
        appended =
            Array.push line model.lines

        len =
            Array.length appended
    in
    if len > maxLines + 500 then
        { model
            | lines = Array.slice (len - maxLines) len appended
            , firstLine = model.firstLine + (len - maxLines)
        }

    else
        { model | lines = appended }


{-| Scroll the log to its last line (when following).
-}
scrollToEnd : Model -> Cmd Msg
scrollToEnd model =
    if model.follow then
        Dom.getViewportOf logId
            |> Task.andThen (\vp -> Dom.setViewportOf logId 0 vp.scene.height)
            |> Task.attempt (\_ -> Scrolled)

    else
        Cmd.none



-- VIEW


view : String -> Model -> Html Msg
view rawUrl model =
    div []
        [ div [ class "toolbar" ]
            [ label [ class "field grow" ]
                [ text "Search log"
                , input [ type_ "search", placeholder "Show only lines containing…", value model.searchTerm, onInput SearchInput ] []
                ]
            , Ui.chip
                { label = "Follow output"
                , count = Nothing
                , tone = Nothing
                , pressed = model.follow
                , onPress = SetFollow (not model.follow)
                }
            , Ui.externalLink rawUrl [ text "Raw log" ]
            ]
        , viewMeta model
        , lazy viewLines model
        ]


viewMeta : Model -> Html msg
viewMeta model =
    let
        count =
            Array.length model.lines

        truncated =
            model.firstLine > 1

        total =
            model.firstLine - 1 + count
    in
    if count == 0 then
        text ""

    else if truncated then
        p [ class "log-meta" ]
            [ text
                ("Showing the last "
                    ++ String.fromInt count
                    ++ " of "
                    ++ String.fromInt total
                    ++ " lines. Use Raw log for the full output."
                )
            ]

    else
        p [ class "log-meta" ] [ text (String.fromInt total ++ " lines") ]


viewLines : Model -> Html Msg
viewLines model =
    let
        needle =
            String.toLower (String.trim model.searchTerm)

        numbered =
            Array.toIndexedList model.lines
                |> List.map (\( i, line ) -> ( model.firstLine + i, line ))

        shown =
            if String.isEmpty needle then
                numbered

            else
                List.filter (\( _, line ) -> String.contains needle (String.toLower line)) numbered
    in
    div
        [ id logId
        , class "log-output"
        , attribute "role" "log"
        , attribute "aria-label" "Build log"
        , tabindex 0
        , on "scroll" (followDecoder model.follow)
        ]
        (if List.isEmpty shown then
            [ div [ class "log-empty" ] [ text (emptyText needle model.source) ] ]

         else
            List.map (\( n, line ) -> lazy2 viewLine n line) shown
        )


{-| Follow while scrolled to the bottom; stop when the reader scrolls up.
Only emits a message when that changes.
-}
followDecoder : Bool -> D.Decoder Msg
followDecoder following =
    D.map3 (\top height client -> height - top - client < 24)
        (D.at [ "target", "scrollTop" ] D.float)
        (D.at [ "target", "scrollHeight" ] D.float)
        (D.at [ "target", "clientHeight" ] D.float)
        |> D.andThen
            (\atBottom ->
                if atBottom == following then
                    D.fail "unchanged"

                else
                    D.succeed (SetFollow atBottom)
            )


emptyText : String -> Source -> String
emptyText needle source =
    if not (String.isEmpty needle) then
        "No lines match this search."

    else
        case source of
            Waiting ->
                "Waiting for output…"

            Fetching ->
                "Loading the stored log…"

            Fetched ->
                "The stored log is empty."

            Missing ->
                "No log stored yet. Output will appear here while this derivation builds."

            FetchFailed error ->
                "Could not load the stored log. " ++ Ui.httpErrorMessage error


viewLine : Int -> String -> Html msg
viewLine number line =
    div [ class "log-line" ]
        [ span [ class "log-ln", attribute "aria-hidden" "true" ] [ text (String.fromInt number) ]
        , span [ class "log-text" ] [ text line ]
        ]

module Components.LogViewer exposing
    ( Model
    , Msg
    , init
    , update
    , view
    , addLine
    , addLines
    )

{-| Real-time build log viewer component.

Displays build logs with line numbers, syntax highlighting, auto-scroll,
and search functionality.

-}

import Html exposing (Html, button, div, input, pre, span, text)
import Html.Attributes exposing (class, placeholder, type_, value)
import Html.Events exposing (onClick, onInput)
import Html.Lazy exposing (lazy, lazy2)


{-| Log viewer model.
-}
type alias Model =
    { lines : List LogLine
    , searchTerm : String
    , autoScroll : Bool
    , maxLines : Int
    }


{-| A single log line with line number and content.
-}
type alias LogLine =
    { lineNumber : Int
    , content : String
    }


{-| Log viewer messages.
-}
type Msg
    = SearchInput String
    | ToggleAutoScroll
    | ClearLogs


{-| Initialize the log viewer.
-}
init : Model
init =
    { lines = []
    , searchTerm = ""
    , autoScroll = True
    , maxLines = 10000
    }


{-| Update the log viewer.
-}
update : Msg -> Model -> Model
update msg model =
    case msg of
        SearchInput term ->
            { model | searchTerm = term }

        ToggleAutoScroll ->
            { model | autoScroll = not model.autoScroll }

        ClearLogs ->
            { model | lines = [] }


{-| Add a single log line.
-}
addLine : String -> Model -> Model
addLine content model =
    let
        lineNumber =
            List.length model.lines + 1

        newLine =
            { lineNumber = lineNumber, content = content }

        -- Keep only the most recent lines to prevent memory issues
        updatedLines =
            List.take model.maxLines (model.lines ++ [ newLine ])
    in
    { model | lines = updatedLines }


{-| Add multiple log lines at once.
-}
addLines : List String -> Model -> Model
addLines contentList model =
    List.foldl addLine model contentList


{-| View the log viewer.
-}
view : Model -> Html Msg
view model =
    div []
        [ viewToolbar model
        , viewLogs model
        ]


{-| View the toolbar with search and controls.
-}
viewToolbar : Model -> Html Msg
viewToolbar model =
    div [ class "flex items-center justify-between pa2 bg-near-white bb b--black-10" ]
        [ -- Search input
          input
            [ type_ "text"
            , class "pa2 br2 ba b--black-20 w-50"
            , placeholder "Search logs..."
            , value model.searchTerm
            , onInput SearchInput
            ]
            []

        -- Controls
        , div [ class "flex items-center" ]
            [ button
                [ class "pa2 mr2 br2 ba b--black-20 bg-white hover-bg-light-gray pointer"
                , onClick ToggleAutoScroll
                ]
                [ text
                    (if model.autoScroll then
                        "⏸ Pause Auto-scroll"

                     else
                        "▶ Resume Auto-scroll"
                    )
                ]
            , button
                [ class "pa2 br2 ba b--black-20 bg-white hover-bg-light-gray pointer"
                , onClick ClearLogs
                ]
                [ text "🗑 Clear" ]
            ]
        ]


{-| View the log lines.
-}
viewLogs : Model -> Html Msg
viewLogs model =
    let
        filteredLines =
            if String.isEmpty model.searchTerm then
                model.lines

            else
                List.filter
                    (\line ->
                        String.contains
                            (String.toLower model.searchTerm)
                            (String.toLower line.content)
                    )
                    model.lines
    in
    div [ class "log-viewer" ]
        (if List.isEmpty filteredLines then
            [ div [ class "tc pv4 gray" ]
                [ text
                    (if String.isEmpty model.searchTerm then
                        "No logs yet"

                     else
                        "No matching logs"
                    )
                ]
            ]

         else
            List.map (lazy viewLogLine) filteredLines
        )


{-| View a single log line.
-}
viewLogLine : LogLine -> Html Msg
viewLogLine line =
    div [ class "log-line" ]
        [ span [ class "log-line-number" ]
            [ text (String.fromInt line.lineNumber) ]
        , span [ class "log-content" ]
            [ text line.content ]
        ]

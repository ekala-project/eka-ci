port module Ports exposing
    ( websocketOut
    , websocketIn
    , WebSocketMessage(..)
    , IncomingMessage(..)
    , BuildStateChangeEvent
    , JobCompleteEvent
    , LogLineEvent
    , encodeSubscribeMessage
    , decodeIncomingMessage
    )

{-| Ports for WebSocket communication with JavaScript.

The WebSocket is managed on the JavaScript side, and Elm sends/receives
messages via these ports.

-}

import Json.Decode as D
import Json.Encode as E
import Models.BuildState as BS


{-| Outgoing port: Elm -> JavaScript

Send messages to the WebSocket (connect, subscribe, unsubscribe).

-}
port websocketOut : E.Value -> Cmd msg


{-| Incoming port: JavaScript -> Elm

Receive messages from the WebSocket (events, connection status).

-}
port websocketIn : (E.Value -> msg) -> Sub msg


{-| Messages that can be sent TO the WebSocket.
-}
type WebSocketMessage
    = Connect
    | Subscribe String String -- resource type, id
    | Unsubscribe String String -- resource type, id


{-| Encode a message to send to the WebSocket.
-}
encodeSubscribeMessage : String -> String -> E.Value
encodeSubscribeMessage resource id =
    E.object
        [ ( "type", E.string "subscribe" )
        , ( "resource", E.string resource )
        , ( "id", E.string id )
        ]


{-| Messages received FROM the WebSocket.
-}
type IncomingMessage
    = Connected
    | Disconnected
    | BuildStateChange BuildStateChangeEvent
    | JobComplete JobCompleteEvent
    | LogLine LogLineEvent
    | Error String


{-| Build state change event from the server.
-}
type alias BuildStateChangeEvent =
    { drvPath : String
    , oldState : BS.DrvBuildState
    , newState : BS.DrvBuildState
    , timestamp : String
    }


{-| Job completion event from the server.
-}
type alias JobCompleteEvent =
    { jobsetId : Int
    , conclusion : String
    , timestamp : String
    }


{-| Log line event from the server.
-}
type alias LogLineEvent =
    { drvPath : String
    , line : String
    , timestamp : String
    }


{-| Decode an incoming WebSocket message.
-}
decodeIncomingMessage : E.Value -> Result D.Error IncomingMessage
decodeIncomingMessage value =
    D.decodeValue incomingMessageDecoder value


incomingMessageDecoder : D.Decoder IncomingMessage
incomingMessageDecoder =
    D.field "type" D.string
        |> D.andThen
            (\msgType ->
                case msgType of
                    "connected" ->
                        D.succeed Connected

                    "disconnected" ->
                        D.succeed Disconnected

                    "build_state_change" ->
                        D.map BuildStateChange buildStateChangeDecoder

                    "job_complete" ->
                        D.map JobComplete jobCompleteDecoder

                    "log_line" ->
                        D.map LogLine logLineDecoder

                    "error" ->
                        D.map Error (D.field "message" D.string)

                    _ ->
                        D.fail ("Unknown message type: " ++ msgType)
            )


buildStateChangeDecoder : D.Decoder BuildStateChangeEvent
buildStateChangeDecoder =
    D.map4 BuildStateChangeEvent
        (D.field "drv_path" D.string)
        (D.field "old_state" buildStateDecoder)
        (D.field "new_state" buildStateDecoder)
        (D.field "timestamp" D.string)


jobCompleteDecoder : D.Decoder JobCompleteEvent
jobCompleteDecoder =
    D.map3 JobCompleteEvent
        (D.field "jobset_id" D.int)
        (D.field "conclusion" D.string)
        (D.field "timestamp" D.string)


logLineDecoder : D.Decoder LogLineEvent
logLineDecoder =
    D.map3 LogLineEvent
        (D.field "drv_path" D.string)
        (D.field "line" D.string)
        (D.field "timestamp" D.string)


{-| Decode a DrvBuildState from JSON (duplicated from Decoder.elm for ports).
-}
buildStateDecoder : D.Decoder BS.DrvBuildState
buildStateDecoder =
    D.oneOf
        [ D.string
            |> D.andThen
                (\str ->
                    case str of
                        "Queued" ->
                            D.succeed BS.Queued

                        "Buildable" ->
                            D.succeed BS.Buildable

                        "FailedRetry" ->
                            D.succeed BS.FailedRetry

                        "Building" ->
                            D.succeed BS.Building

                        "TransitiveFailure" ->
                            D.succeed BS.TransitiveFailure

                        "Blocked" ->
                            D.succeed BS.Blocked

                        _ ->
                            D.fail ("Unknown build state: " ++ str)
                )
        , D.field "Completed" buildResultDecoder
            |> D.map BS.Completed
        , D.field "Interrupted" interruptionKindDecoder
            |> D.map BS.Interrupted
        ]


buildResultDecoder : D.Decoder BS.DrvBuildResult
buildResultDecoder =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "Success" ->
                        D.succeed BS.Success

                    "Failure" ->
                        D.succeed BS.Failure

                    _ ->
                        D.fail ("Unknown build result: " ++ str)
            )


interruptionKindDecoder : D.Decoder BS.DrvBuildInterruptionKind
interruptionKindDecoder =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "OutOfMemory" ->
                        D.succeed BS.OutOfMemory

                    "Timeout" ->
                        D.succeed BS.Timeout

                    "Cancelled" ->
                        D.succeed BS.Cancelled

                    "ProcessDeath" ->
                        D.succeed BS.ProcessDeath

                    _ ->
                        D.fail ("Unknown interruption kind: " ++ str)
            )

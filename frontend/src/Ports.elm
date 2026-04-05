port module Ports exposing
    ( BuildStateChangeEvent
    , IncomingMessage(..)
    , JobCompleteEvent
    , JobStatsUpdateEvent
    , LogLineEvent
    , WebSocketMessage(..)
    , clearToken
    , decodeIncomingMessage
    , encodeSubscribeMessage
    , encodeUnsubscribeMessage
    , storeToken
    , tokenReceived
    , websocketIn
    , websocketOut
    )

{-| Ports for WebSocket communication and localStorage management.

WebSocket: Managed on the JavaScript side, Elm sends/receives messages via ports.
LocalStorage: JWT token persistence for authentication.

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


{-| Encode a subscribe message to send to the WebSocket.
-}
encodeSubscribeMessage : String -> String -> E.Value
encodeSubscribeMessage resource id =
    E.object
        [ ( "type", E.string "subscribe" )
        , ( "resource", E.string resource )
        , ( "id", E.string id )
        ]


{-| Encode an unsubscribe message to send to the WebSocket.
-}
encodeUnsubscribeMessage : String -> String -> E.Value
encodeUnsubscribeMessage resource id =
    E.object
        [ ( "type", E.string "unsubscribe" )
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
    | JobStatsUpdate JobStatsUpdateEvent
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


{-| Job statistics update event from the server.
-}
type alias JobStatsUpdateEvent =
    { jobsetId : Int
    , totalDrvs : Int
    , queuedDrvs : Int
    , buildableDrvs : Int
    , buildingDrvs : Int
    , completedSuccessDrvs : Int
    , completedFailureDrvs : Int
    , failedRetryDrvs : Int
    , transitiveFailureDrvs : Int
    , blockedDrvs : Int
    , interruptedDrvs : Int
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

                    "job_stats_update" ->
                        D.map JobStatsUpdate jobStatsUpdateDecoder

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


jobStatsUpdateDecoder : D.Decoder JobStatsUpdateEvent
jobStatsUpdateDecoder =
    D.succeed JobStatsUpdateEvent
        |> andMap (D.field "jobset_id" D.int)
        |> andMap (D.field "total_drvs" D.int)
        |> andMap (D.field "queued_drvs" D.int)
        |> andMap (D.field "buildable_drvs" D.int)
        |> andMap (D.field "building_drvs" D.int)
        |> andMap (D.field "completed_success_drvs" D.int)
        |> andMap (D.field "completed_failure_drvs" D.int)
        |> andMap (D.field "failed_retry_drvs" D.int)
        |> andMap (D.field "transitive_failure_drvs" D.int)
        |> andMap (D.field "blocked_drvs" D.int)
        |> andMap (D.field "interrupted_drvs" D.int)
        |> andMap (D.field "timestamp" D.string)


andMap : D.Decoder a -> D.Decoder (a -> b) -> D.Decoder b
andMap =
    D.map2 (|>)


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



{- LocalStorage Ports -}


{-| Store JWT token in localStorage.

Send a token to JavaScript to persist in localStorage.

-}
port storeToken : String -> Cmd msg


{-| Clear JWT token from localStorage.

Tell JavaScript to remove the token.

-}
port clearToken : () -> Cmd msg


{-| Receive token from localStorage on app init.

JavaScript will send the stored token (if any) when the app starts.

-}
port tokenReceived : (Maybe String -> msg) -> Sub msg

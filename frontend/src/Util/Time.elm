module Util.Time exposing (formatIso)

{-| Time formatting utilities for the EkaCI frontend.
-}


{-| Format an RFC 3339 / ISO 8601 timestamp from the API for display.

The API returns GitHub timestamps (`2026-11-18T14:03:27Z`) and SQLite
`CURRENT_TIMESTAMP` values (`2026-11-18 14:03:27`, UTC). Both render as
`2026-11-18 14:03 UTC`; a non-UTC offset is kept (`… 14:03 +02:00`).
Anything that does not look like a timestamp is returned unchanged.

    formatIso "2026-11-18T14:03:27Z"
    --> "2026-11-18 14:03 UTC"

-}
formatIso : String -> String
formatIso raw =
    let
        separator =
            String.slice 10 11 raw

        looksLikeTimestamp =
            (String.length raw >= 16)
                && (separator == "T" || separator == " ")
                && (String.slice 4 5 raw == "-")
                && (String.slice 13 14 raw == ":")
    in
    if looksLikeTimestamp then
        String.left 10 raw ++ " " ++ String.slice 11 16 raw ++ " " ++ zoneLabel raw

    else
        raw


{-| The zone part after `HH:MM:SS[.fff]`, as "UTC" or the literal offset.
-}
zoneLabel : String -> String
zoneLabel raw =
    let
        zone =
            String.dropLeft 19 raw
                |> dropFraction
    in
    if zone == "" || zone == "Z" || zone == "+00:00" || zone == "+00" then
        "UTC"

    else
        zone


dropFraction : String -> String
dropFraction rest =
    if String.startsWith "." rest then
        String.dropLeft 1 rest
            |> String.toList
            |> dropWhileDigit
            |> String.fromList

    else
        rest


dropWhileDigit : List Char -> List Char
dropWhileDigit chars =
    case chars of
        c :: cs ->
            if Char.isDigit c then
                dropWhileDigit cs

            else
                chars

        [] ->
            []

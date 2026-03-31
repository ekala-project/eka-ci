module Util.Time exposing
    ( formatAbsolute
    , formatDuration
    , formatRelative
    )

{-| Time formatting utilities for the EkaCI frontend.

This module provides functions to format timestamps and durations
in human-readable formats.

-}

import Time exposing (Posix)


{-| Format a timestamp as a relative time string.

    formatRelative currentTime pastTime
    --> "2 hours ago"

    formatRelative currentTime recentTime
    --> "just now"

-}
formatRelative : Posix -> Posix -> String
formatRelative now then_ =
    let
        diff =
            Time.posixToMillis now - Time.posixToMillis then_

        seconds =
            diff // 1000

        minutes =
            seconds // 60

        hours =
            minutes // 60

        days =
            hours // 24
    in
    if seconds < 10 then
        "just now"

    else if seconds < 60 then
        String.fromInt seconds ++ " seconds ago"

    else if minutes < 60 then
        if minutes == 1 then
            "1 minute ago"

        else
            String.fromInt minutes ++ " minutes ago"

    else if hours < 24 then
        if hours == 1 then
            "1 hour ago"

        else
            String.fromInt hours ++ " hours ago"

    else if days == 1 then
        "1 day ago"

    else if days < 7 then
        String.fromInt days ++ " days ago"

    else if days < 30 then
        let
            weeks =
                days // 7
        in
        if weeks == 1 then
            "1 week ago"

        else
            String.fromInt weeks ++ " weeks ago"

    else if days < 365 then
        let
            months =
                days // 30
        in
        if months == 1 then
            "1 month ago"

        else
            String.fromInt months ++ " months ago"

    else
        let
            years =
                days // 365
        in
        if years == 1 then
            "1 year ago"

        else
            String.fromInt years ++ " years ago"


{-| Format a timestamp as an absolute time string.

    formatAbsolute zone time
    --> "2024-03-11 14:30:00"

-}
formatAbsolute : Time.Zone -> Posix -> String
formatAbsolute zone time =
    let
        year =
            String.fromInt (Time.toYear zone time)

        month =
            String.fromInt (monthToNumber (Time.toMonth zone time))
                |> String.padLeft 2 '0'

        day =
            String.fromInt (Time.toDay zone time)
                |> String.padLeft 2 '0'

        hour =
            String.fromInt (Time.toHour zone time)
                |> String.padLeft 2 '0'

        minute =
            String.fromInt (Time.toMinute zone time)
                |> String.padLeft 2 '0'

        second =
            String.fromInt (Time.toSecond zone time)
                |> String.padLeft 2 '0'
    in
    year ++ "-" ++ month ++ "-" ++ day ++ " " ++ hour ++ ":" ++ minute ++ ":" ++ second


{-| Format a duration in milliseconds as a human-readable string.

    formatDuration 125000
    --> "2m 5s"

    formatDuration 3661000
    --> "1h 1m 1s"

-}
formatDuration : Int -> String
formatDuration millis =
    let
        totalSeconds =
            millis // 1000

        seconds =
            modBy 60 totalSeconds

        totalMinutes =
            totalSeconds // 60

        minutes =
            modBy 60 totalMinutes

        hours =
            totalMinutes // 60
    in
    if hours > 0 then
        String.fromInt hours
            ++ "h "
            ++ String.fromInt minutes
            ++ "m "
            ++ String.fromInt seconds
            ++ "s"

    else if minutes > 0 then
        String.fromInt minutes ++ "m " ++ String.fromInt seconds ++ "s"

    else if seconds > 0 then
        String.fromInt seconds ++ "s"

    else
        "0s"


{-| Convert Time.Month to a number (1-12).
-}
monthToNumber : Time.Month -> Int
monthToNumber month =
    case month of
        Time.Jan ->
            1

        Time.Feb ->
            2

        Time.Mar ->
            3

        Time.Apr ->
            4

        Time.May ->
            5

        Time.Jun ->
            6

        Time.Jul ->
            7

        Time.Aug ->
            8

        Time.Sep ->
            9

        Time.Oct ->
            10

        Time.Nov ->
            11

        Time.Dec ->
            12

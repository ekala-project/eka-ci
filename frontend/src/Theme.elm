module Theme exposing
    ( Theme(..)
    , fromString
    , label
    , next
    , toString
    )

{-| Colour theme preference.

`System` follows the OS (`prefers-color-scheme`); `Light` and `Dark` set
`data-theme` on `<html>` via the `setTheme` port. `static/index.html`
re-applies a saved override before first paint.

-}


type Theme
    = System
    | Light
    | Dark


{-| Parse the value stored by `static/ports.js`; anything unknown is System.
-}
fromString : String -> Theme
fromString value =
    case value of
        "light" ->
            Light

        "dark" ->
            Dark

        _ ->
            System


toString : Theme -> String
toString theme =
    case theme of
        System ->
            "system"

        Light ->
            "light"

        Dark ->
            "dark"


label : Theme -> String
label theme =
    case theme of
        System ->
            "System"

        Light ->
            "Light"

        Dark ->
            "Dark"


{-| The header toggle cycles System → Light → Dark → System.
-}
next : Theme -> Theme
next theme =
    case theme of
        System ->
            Light

        Light ->
            Dark

        Dark ->
            System

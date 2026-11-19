module Components.Ui exposing
    ( Head
    , Tone(..)
    , avatar
    , avatarLarge
    , badge
    , chip
    , dataTip
    , dismissible
    , drvName
    , emptyNote
    , emptyState
    , errorPage
    , externalLink
    , facts
    , httpError
    , httpErrorMessage
    , liveBadge
    , loading
    , loadingInline
    , messages
    , notice
    , pageHead
    , panel
    , shortSha
    , simpleHead
    , tableWrap
    , tag
    , timestamp
    , toneClass
    , toneName
    )

{-| Shared view building blocks for the EkaCI design system.

Class names map onto `static/styles/main.css`. Status colour always comes
with a text label (badges, legends), never on its own.

-}

import Html exposing (Attribute, Html, a, button, dd, div, dl, dt, h1, h2, img, li, nav, ol, p, section, span, text)
import Html.Attributes exposing (alt, attribute, class, href, rel, src, target, title, type_)
import Html.Events exposing (onClick)
import Http
import Util.Time



-- TONES


{-| Reserved status colours.
-}
type Tone
    = Good
    | Warn
    | Serious
    | Critical
    | Info
    | Neutral


toneName : Tone -> String
toneName tone =
    case tone of
        Good ->
            "good"

        Warn ->
            "warn"

        Serious ->
            "serious"

        Critical ->
            "critical"

        Info ->
            "info"

        Neutral ->
            "neutral"


toneClass : Tone -> String
toneClass tone =
    "tone-" ++ toneName tone


{-| A pill with a coloured status dot and a text label.
-}
badge : Tone -> String -> Html msg
badge tone label =
    span [ class ("badge " ++ toneClass tone) ]
        [ span [ class "dot", attribute "aria-hidden" "true" ] []
        , text label
        ]


{-| A badge whose dot pulses, for work in progress.
-}
liveBadge : Tone -> String -> Html msg
liveBadge tone label =
    span [ class ("badge is-live " ++ toneClass tone) ]
        [ span [ class "dot", attribute "aria-hidden" "true" ] []
        , text label
        ]


{-| A small neutral label, e.g. "FOD".
-}
tag : String -> Html msg
tag label =
    span [ class "tag" ] [ text label ]


{-| Hover tooltip text. Only an enhancement: the value must also be printed.
-}
dataTip : String -> Attribute msg
dataTip =
    attribute "data-tip"



-- PAGE STRUCTURE


{-| Page header: breadcrumb trail (ancestors only, as label/href pairs),
title content, a lede line and right-aligned actions.
-}
type alias Head msg =
    { crumbs : List ( String, String )
    , title : List (Html msg)
    , lede : List (Html msg)
    , actions : List (Html msg)
    }


pageHead : Head msg -> Html msg
pageHead head =
    div [ class "page-head" ]
        [ breadcrumb head.crumbs
        , div [ class "page-head-row" ]
            [ div []
                [ div [ class "title-badge" ] head.title
                , if List.isEmpty head.lede then
                    text ""

                  else
                    p [] head.lede
                ]
            , if List.isEmpty head.actions then
                text ""

              else
                div [ class "actions" ] head.actions
            ]
        ]


{-| A page header with just a title and a plain-text lede.
-}
simpleHead : String -> String -> Html msg
simpleHead heading lede =
    pageHead
        { crumbs = []
        , title = [ h1 [] [ text heading ] ]
        , lede =
            if String.isEmpty lede then
                []

            else
                [ text lede ]
        , actions = []
        }


breadcrumb : List ( String, String ) -> Html msg
breadcrumb crumbs =
    if List.isEmpty crumbs then
        text ""

    else
        nav [ class "breadcrumb", attribute "aria-label" "Breadcrumb" ]
            [ ol []
                (List.map
                    (\( label, url ) -> li [] [ a [ href url ] [ text label ] ])
                    crumbs
                )
            ]


{-| A bordered panel with an h2 heading and optional right-hand content.
-}
panel : String -> List (Html msg) -> List (Html msg) -> Html msg
panel heading aside children =
    section [ class "panel" ]
        (div [ class "panel-head" ] (h2 [] [ text heading ] :: aside)
            :: children
        )


tableWrap : Html msg -> Html msg
tableWrap table =
    div [ class "table-wrap" ] [ table ]


{-| Definition list of label/value rows.
-}
facts : List ( String, Html msg ) -> Html msg
facts rows =
    dl [ class "facts" ]
        (List.map (\( label, value ) -> div [] [ dt [] [ text label ], dd [] [ value ] ]) rows)



-- STATES


{-| Top-level empty state (dashed box with a heading).
-}
emptyState : String -> String -> Html msg
emptyState heading body =
    div [ class "empty-state" ]
        [ h2 [] [ text heading ]
        , p [] [ text body ]
        ]


{-| Empty state inside a panel (the panel already has a heading).
-}
emptyNote : String -> Html msg
emptyNote body =
    div [ class "empty-state" ] [ p [] [ text body ] ]


loading : String -> Html msg
loading message =
    div [ class "loading", attribute "role" "status" ]
        [ span [ class "spinner", attribute "aria-hidden" "true" ] []
        , text message
        ]


loadingInline : String -> Html msg
loadingInline message =
    div [ class "loading inline", attribute "role" "status" ]
        [ span [ class "spinner", attribute "aria-hidden" "true" ] []
        , text message
        ]


{-| Full-page error: a large status code, heading, explanation and one action.
-}
errorPage : { code : String, heading : String, body : String, action : Html msg } -> Html msg
errorPage config =
    section [ class "error-page" ]
        [ if String.isEmpty config.code then
            text ""

          else
            p [ class "error-code" ] [ text config.code ]
        , h1 [] [ text config.heading ]
        , p [] [ text config.body ]
        , config.action
        ]


notice : Tone -> List (Html msg) -> Html msg
notice tone children =
    div [ class ("notice " ++ toneClass tone), attribute "role" (noticeRole tone) ] children


noticeRole : Tone -> String
noticeRole tone =
    if tone == Critical then
        "alert"

    else
        "status"


{-| A notice with a Dismiss button.
-}
dismissible : Tone -> msg -> String -> Html msg
dismissible tone onDismiss message =
    div [ class ("notice dismissible " ++ toneClass tone), attribute "role" (noticeRole tone) ]
        [ div [ class "notice-body" ] [ text message ]
        , button [ type_ "button", class "notice-dismiss", onClick onDismiss ] [ text "Dismiss" ]
        ]


{-| Page-level success / error messages, stacked, each dismissible.
-}
messages : { error : Maybe String, success : Maybe String, onDismiss : msg } -> Html msg
messages config =
    let
        items =
            List.filterMap identity
                [ Maybe.map (dismissible Critical config.onDismiss) config.error
                , Maybe.map (dismissible Good config.onDismiss) config.success
                ]
    in
    if List.isEmpty items then
        text ""

    else
        div [ class "notice-stack" ] items


{-| An HTTP failure as a critical notice with a short heading.
-}
httpError : String -> Http.Error -> Html msg
httpError heading error =
    notice Critical
        [ Html.strong [] [ text heading ]
        , text (" " ++ httpErrorMessage error)
        ]


httpErrorMessage : Http.Error -> String
httpErrorMessage error =
    case error of
        Http.BadUrl url ->
            "Invalid URL: " ++ url

        Http.Timeout ->
            "The request timed out. Try again."

        Http.NetworkError ->
            "Could not reach the server. Check your connection."

        Http.BadStatus 401 ->
            "You need to sign in again (401)."

        Http.BadStatus 403 ->
            "You don't have permission to do that (403)."

        Http.BadStatus 404 ->
            "Not found (404)."

        Http.BadStatus status ->
            "The server returned an error (" ++ String.fromInt status ++ ")."

        Http.BadBody body ->
            "Unexpected response: " ++ body



-- CONTROLS


{-| A toggle chip (filter button) with an optional count and status dot.
-}
chip : { label : String, count : Maybe Int, tone : Maybe Tone, pressed : Bool, onPress : msg } -> Html msg
chip config =
    button
        [ type_ "button"
        , class "chip"
        , attribute "aria-pressed" (boolString config.pressed)
        , onClick config.onPress
        ]
        (List.filterMap identity
            [ Maybe.map
                (\tone -> span [ class ("dot " ++ toneClass tone), attribute "aria-hidden" "true" ] [])
                config.tone
            , Just (text config.label)
            , Maybe.map
                (\n -> span [ class "chip-count" ] [ text (String.fromInt n) ])
                config.count
            ]
        )


boolString : Bool -> String
boolString value =
    if value then
        "true"

    else
        "false"


externalLink : String -> List (Html msg) -> Html msg
externalLink url children =
    a [ href url, target "_blank", rel "noopener noreferrer" ] children



-- SMALL FORMATTERS


{-| A `<time>` element: readable text, the raw value on hover.
-}
timestamp : String -> Html msg
timestamp raw =
    Html.node "time"
        [ attribute "datetime" raw, title raw ]
        [ text (Util.Time.formatIso raw) ]


{-| A round avatar, or the user's initial when there is no image.
It is decorative: the username is always shown next to it.
-}
avatar : Maybe String -> String -> Html msg
avatar maybeUrl username =
    avatarWith "avatar" maybeUrl username


avatarLarge : Maybe String -> String -> Html msg
avatarLarge maybeUrl username =
    avatarWith "avatar avatar-lg" maybeUrl username


avatarWith : String -> Maybe String -> String -> Html msg
avatarWith classes maybeUrl username =
    case maybeUrl of
        Just url ->
            if String.isEmpty url then
                initialAvatar classes username

            else
                img [ class classes, src url, alt "", attribute "loading" "lazy" ] []

        Nothing ->
            initialAvatar classes username


initialAvatar : String -> String -> Html msg
initialAvatar classes username =
    span [ class classes, attribute "aria-hidden" "true" ]
        [ text (String.toUpper (String.left 1 username)) ]


shortSha : String -> String
shortSha sha =
    String.left 8 sha


{-| `hash-name-1.0.drv` → `name-1.0`, for derivations the API has no name for.
-}
drvName : String -> String
drvName drvPath =
    let
        base =
            drvPath |> String.split "/" |> List.reverse |> List.head |> Maybe.withDefault drvPath

        withoutHash =
            case String.indexes "-" base of
                i :: _ ->
                    String.dropLeft (i + 1) base

                [] ->
                    base
    in
    if String.endsWith ".drv" withoutHash then
        String.dropRight 4 withoutHash

    else
        withoutHash

module Components.DrvTree exposing
    ( Model
    , Msg
    , init
    , setDependencies
    , update
    , view
    )

{-| Derivation dependency tree component.

Displays a hierarchical tree view of derivation dependencies
with expand/collapse functionality.

-}

import Components.StatusBadge as StatusBadge
import Dict exposing (Dict)
import Html exposing (Html, a, button, div, span, text)
import Html.Attributes exposing (class, href)
import Html.Events exposing (onClick)
import Models.BuildState as BS
import Models.Derivation exposing (DrvDependency)
import Route
import Set exposing (Set)


{-| Tree model tracking expanded nodes.
-}
type alias Model =
    { expanded : Set String
    , dependencies : List DrvDependency
    }


{-| Tree messages.
-}
type Msg
    = ToggleExpand String


{-| Initialize the tree.
-}
init : Model
init =
    { expanded = Set.empty
    , dependencies = []
    }


{-| Set the dependencies to display.
-}
setDependencies : List DrvDependency -> Model -> Model
setDependencies deps model =
    { model | dependencies = deps }


{-| Update the tree.
-}
update : Msg -> Model -> Model
update msg model =
    case msg of
        ToggleExpand drvPath ->
            if Set.member drvPath model.expanded then
                { model | expanded = Set.remove drvPath model.expanded }

            else
                { model | expanded = Set.insert drvPath model.expanded }


{-| View the dependency tree.
-}
view : Model -> Html Msg
view model =
    if List.isEmpty model.dependencies then
        div [ class "tc pv4 gray f6" ]
            [ text "No dependencies" ]

    else
        div [ class "drv-tree" ]
            (List.map (viewDependency model 0) model.dependencies)


{-| View a single dependency node.
-}
viewDependency : Model -> Int -> DrvDependency -> Html Msg
viewDependency model depth dep =
    let
        isExpanded =
            Set.member dep.drvPath model.expanded

        indentClass =
            "pl" ++ String.fromInt (depth * 3)

        expandIcon =
            if isExpanded then
                "▼"

            else
                "▶"
    in
    div []
        [ div [ class ("flex items-center pv2 " ++ indentClass) ]
            [ -- Expand/collapse button (placeholder for future children)
              button
                [ class "bn bg-transparent pointer mr2 f6 gray"
                , onClick (ToggleExpand dep.drvPath)
                ]
                [ text expandIcon ]

            -- Status badge
            , div [ class "mr2" ]
                [ StatusBadge.view dep.buildState ]

            -- Derivation name
            , a
                [ href (Route.toHref (Route.Drv dep.drvPath))
                , class "link blue hover-dark-blue monospace f6"
                ]
                [ text dep.name ]

            -- Derivation path (truncated)
            , span [ class "ml2 gray f7 truncate-path" ]
                [ text dep.drvPath ]
            ]

        -- Child dependencies would be rendered here if we had them
        -- For now, we only have a flat list from the API
        ]

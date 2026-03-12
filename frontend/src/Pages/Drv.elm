module Pages.Drv exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Derivation details page.

Shows comprehensive information about a specific derivation including:
- Derivation metadata
- Build status
- Dependencies tree
- Build logs (if available)

-}

import Api.Api as Api
import Components.DrvTree as DrvTree
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.LogViewer as LogViewer
import Components.StatusBadge as StatusBadge
import Html exposing (Html, div, h1, h2, p, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class)
import Http
import Models.BuildState as BS
import Models.Derivation exposing (DrvDependency, DrvDetails)


{-| Page model.
-}
type Model
    = Loading String
    | Loaded DrvData
    | Failed Http.Error


{-| Derivation data combining details, dependencies, and logs.
-}
type alias DrvData =
    { details : DrvDetails
    , dependencies : List DrvDependency
    , logViewer : LogViewer.Model
    , drvTree : DrvTree.Model
    }


{-| Page messages.
-}
type Msg
    = GotDrvDetails (Result Http.Error DrvDetails)
    | GotDrvDependencies (Result Http.Error (List DrvDependency))
    | LogViewerMsg LogViewer.Msg
    | DrvTreeMsg DrvTree.Msg


{-| Initialize the page with a derivation path.
-}
init : String -> ( Model, Cmd Msg )
init drvPath =
    ( Loading drvPath
    , Cmd.batch
        [ Api.getDrvDetails drvPath GotDrvDetails
        , Api.getDrvDependencies drvPath GotDrvDependencies
        ]
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotDrvDetails result ->
            case result of
                Ok details ->
                    case model of
                        Loading _ ->
                            ( Loaded
                                { details = details
                                , dependencies = []
                                , logViewer = LogViewer.init
                                , drvTree = DrvTree.init
                                }
                            , Cmd.none
                            )

                        Loaded data ->
                            ( Loaded { data | details = details }
                            , Cmd.none
                            )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )

        GotDrvDependencies result ->
            case result of
                Ok deps ->
                    case model of
                        Loaded data ->
                            let
                                updatedTree =
                                    DrvTree.setDependencies deps data.drvTree
                            in
                            ( Loaded
                                { data
                                    | dependencies = deps
                                    , drvTree = updatedTree
                                }
                            , Cmd.none
                            )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )

        LogViewerMsg logMsg ->
            case model of
                Loaded data ->
                    ( Loaded { data | logViewer = LogViewer.update logMsg data.logViewer }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )

        DrvTreeMsg treeMsg ->
            case model of
                Loaded data ->
                    ( Loaded { data | drvTree = DrvTree.update treeMsg data.drvTree }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading drvPath ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4 monospace truncate" ]
                    [ text drvPath ]
                , Loader.viewWithMessage "Loading derivation..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Derivation" ]
                , ErrorView.viewHttp error
                ]

        Loaded data ->
            viewDrvDetails data


{-| View derivation details.
-}
viewDrvDetails : DrvData -> Html Msg
viewDrvDetails data =
    div [ class "pa4" ]
        [ -- Header
          viewDrvHeader data.details

        -- Metadata
        , div [ class "mb4" ]
            [ viewMetadataCard data.details ]

        -- Dependencies
        , div [ class "mb4" ]
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Dependencies" ]
            , div [ class "card pa3" ]
                [ Html.map DrvTreeMsg (DrvTree.view data.drvTree) ]
            ]

        -- Build logs
        , div []
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Build Logs" ]
            , Html.map LogViewerMsg (LogViewer.view data.logViewer)
            ]
        ]


{-| View derivation header.
-}
viewDrvHeader : DrvDetails -> Html Msg
viewDrvHeader details =
    div [ class "mb4" ]
        [ h1 [ class "f2 fw6 mb2" ]
            [ text details.name ]
        , div [ class "flex items-center mb2" ]
            [ StatusBadge.view details.buildState ]
        , p [ class "monospace f7 gray truncate-path" ]
            [ text details.drvPath ]
        ]


{-| View metadata card.
-}
viewMetadataCard : DrvDetails -> Html Msg
viewMetadataCard details =
    div [ class "card" ]
        [ table [ class "table w-100" ]
            [ tbody []
                [ viewMetadataRow "Name" details.name
                , viewMetadataRow "System" details.system
                , viewMetadataRow "Status" (statusText details.buildState)
                , viewMetadataRow "Fixed-output derivation" (boolText details.isFod)
                , viewMetadataRow "Prefer local build" (boolText details.preferLocalBuild)
                , viewMetadataRow "Derivation path" details.drvPath
                ]
            ]
        ]


{-| View a metadata row.
-}
viewMetadataRow : String -> String -> Html Msg
viewMetadataRow label value =
    tr []
        [ th [ class "w-30" ] [ text label ]
        , td [ class "monospace f6" ] [ text value ]
        ]


{-| Convert build state to text.
-}
statusText : BS.DrvBuildState -> String
statusText state =
    BS.toString state


{-| Convert boolean to text.
-}
boolText : Bool -> String
boolText value =
    if value then
        "Yes"

    else
        "No"

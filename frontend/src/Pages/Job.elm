module Pages.Job exposing
    ( Model(..)
    , Msg(..)
    , init
    , update
    , view
    )

{-| Job details page showing jobset information and all derivations.

Displays comprehensive information about a specific job including:

  - Job metadata (SHA, name, repository)
  - Overall progress
  - List of all derivations with their states
  - Maintainers of the attr path
  - Real-time updates via WebSocket

-}

import Api.Api as Api
import Auth exposing (AuthState)
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.ProgressBar as ProgressBar
import Components.StatusBadge as StatusBadge
import Html exposing (Html, a, button, code, div, h1, h2, img, p, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (class, disabled, href, src, title)
import Html.Events exposing (onClick)
import Http
import Json.Encode as E
import Models.BuildState as BS
import Models.Job exposing (JobSetDetails, JobSetDrv)
import Models.Maintainer exposing (MaintainerDetail)
import Ports
import Route


{-| Page model.
-}
type Model
    = Loading String Int AuthState
    | Loaded JobData
    | Failed Http.Error


{-| Job data combining details and derivations.
-}
type alias JobData =
    { jobsetId : Int
    , details : JobSetDetails
    , drvs : List JobSetDrv
    , maintainers : List MaintainerDetail
    , loadingMaintainers : Bool
    , requestingAccess : Bool
    , successMessage : Maybe String
    , errorMessage : Maybe String
    , apiBaseUrl : String
    , authState : AuthState
    }


{-| Page messages.
-}
type Msg
    = GotJobSetDetails (Result Http.Error JobSetDetails)
    | GotJobSetDrvs (Result Http.Error (List JobSetDrv))
    | GotMaintainers (Result Http.Error (List MaintainerDetail))
    | RequestMaintainerAccess
    | MaintainerRequestCompleted (Result Http.Error ())
    | DismissMessage
    | BuildStateChanged Ports.BuildStateChangeEvent
    | JobCompleted Ports.JobCompleteEvent


{-| Initialize the page with a jobset ID.
-}
init : String -> Int -> AuthState -> ( Model, Cmd Msg )
init apiBaseUrl jobsetId authState =
    ( Loading apiBaseUrl jobsetId authState
    , Cmd.batch
        [ Api.getJobSetDetails apiBaseUrl jobsetId GotJobSetDetails
        , Api.getJobSetDrvs apiBaseUrl jobsetId GotJobSetDrvs
        , Api.getJobMaintainers apiBaseUrl jobsetId GotMaintainers
        , Ports.websocketOut (Ports.encodeSubscribeMessage "job" (String.fromInt jobsetId))
        ]
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotJobSetDetails result ->
            case result of
                Ok details ->
                    case model of
                        Loading apiBaseUrl jobsetId authState ->
                            -- Wait for drvs to load too
                            ( Loaded
                                { jobsetId = jobsetId
                                , details = details
                                , drvs = []
                                , maintainers = []
                                , loadingMaintainers = True
                                , requestingAccess = False
                                , successMessage = Nothing
                                , errorMessage = Nothing
                                , apiBaseUrl = apiBaseUrl
                                , authState = authState
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

        GotJobSetDrvs result ->
            case result of
                Ok drvs ->
                    case model of
                        Loaded data ->
                            ( Loaded { data | drvs = drvs }
                            , Cmd.none
                            )

                        Loading _ _ _ ->
                            -- Details haven't loaded yet, create partial state
                            ( model, Cmd.none )

                        _ ->
                            ( model, Cmd.none )

                Err error ->
                    ( Failed error, Cmd.none )

        GotMaintainers result ->
            case model of
                Loaded data ->
                    case result of
                        Ok maintainers ->
                            ( Loaded
                                { data
                                    | maintainers = maintainers
                                    , loadingMaintainers = False
                                }
                            , Cmd.none
                            )

                        Err _ ->
                            ( Loaded { data | loadingMaintainers = False }
                            , Cmd.none
                            )

                _ ->
                    ( model, Cmd.none )

        RequestMaintainerAccess ->
            case model of
                Loaded data ->
                    case Auth.getToken data.authState of
                        Just token ->
                            ( Loaded { data | requestingAccess = True, errorMessage = Nothing }
                            , requestMaintainerAccess data.apiBaseUrl token data.details.jobName
                            )

                        Nothing ->
                            ( Loaded { data | errorMessage = Just "You must be logged in to request maintainer access" }
                            , Cmd.none
                            )

                _ ->
                    ( model, Cmd.none )

        MaintainerRequestCompleted result ->
            case model of
                Loaded data ->
                    case result of
                        Ok _ ->
                            ( Loaded
                                { data
                                    | requestingAccess = False
                                    , successMessage = Just "Maintainer access requested successfully!"
                                    , errorMessage = Nothing
                                }
                            , Api.getJobMaintainers data.apiBaseUrl data.jobsetId GotMaintainers
                            )

                        Err error ->
                            ( Loaded
                                { data
                                    | requestingAccess = False
                                    , errorMessage = Just (httpErrorToString error)
                                }
                            , Cmd.none
                            )

                _ ->
                    ( model, Cmd.none )

        DismissMessage ->
            case model of
                Loaded data ->
                    ( Loaded { data | successMessage = Nothing, errorMessage = Nothing }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )

        BuildStateChanged event ->
            case model of
                Loaded data ->
                    let
                        updatedDrvs =
                            updateDrvState event.drvPath event.newState data.drvs

                        updatedDetails =
                            recalculateStats updatedDrvs data.details
                    in
                    ( Loaded
                        { data
                            | drvs = updatedDrvs
                            , details = updatedDetails
                        }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )

        JobCompleted event ->
            -- Job completed, could refetch or show notification
            case model of
                Loaded data ->
                    if data.jobsetId == event.jobsetId then
                        -- Refresh job details to get final state
                        ( model
                        , Api.getJobSetDetails data.apiBaseUrl data.jobsetId GotJobSetDetails
                        )

                    else
                        ( model, Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| View the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading _ jobsetId _ ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text ("Job #" ++ String.fromInt jobsetId) ]
                , Loader.viewWithMessage "Loading job details..."
                ]

        Failed error ->
            div [ class "pa4" ]
                [ h1 [ class "f2 fw6 mb4" ]
                    [ text "Job" ]
                , ErrorView.viewHttp error
                ]

        Loaded data ->
            viewJobDetails data


{-| View job details.
-}
viewJobDetails : JobData -> Html Msg
viewJobDetails data =
    div [ class "pa4" ]
        [ -- Header
          viewJobHeader data.details

        -- Messages
        , viewMessages data

        -- Progress
        , viewProgress data.details

        -- Maintainers section
        , div [ class "mt4" ]
            [ viewMaintainersSection data ]

        -- Derivations table
        , div [ class "mt4" ]
            [ h2 [ class "f4 fw6 mb3" ]
                [ text "Derivations" ]
            , viewDrvTable data.drvs
            ]
        ]


{-| View job header with metadata.
-}
viewJobHeader : JobSetDetails -> Html Msg
viewJobHeader details =
    div [ class "mb4" ]
        [ h1 [ class "f2 fw6 mb2" ]
            [ text details.jobName ]
        , div [ class "flex items-center gray f6" ]
            [ a
                [ href (Route.toHref (Route.Repository details.owner details.repoName))
                , class "link blue hover-dark-blue mr3"
                ]
                [ text (details.owner ++ "/" ++ details.repoName) ]
            , span [ class "mr3" ] [ text "•" ]
            , a
                [ href (Route.toHref (Route.Commit details.sha))
                , class "link blue hover-dark-blue monospace"
                ]
                [ text (String.left 8 details.sha) ]
            ]
        ]


{-| View overall progress bar.
-}
viewProgress : JobSetDetails -> Html Msg
viewProgress details =
    let
        segments =
            ProgressBar.fromJobStats
                { completed = details.completedDrvs
                , failed = details.failedDrvs + details.transitiveFailureDrvs
                , building = details.buildingDrvs
                , queued = details.queuedDrvs + details.buildableDrvs
                }
    in
    div [ class "card pa3" ]
        [ div [ class "mb3 flex items-center justify-between" ]
            [ h2 [ class "f5 fw6 ma0" ]
                [ text "Progress" ]
            , p [ class "f6 gray ma0" ]
                [ text
                    (String.fromInt details.completedDrvs
                        ++ " / "
                        ++ String.fromInt details.totalDrvs
                        ++ " completed"
                    )
                ]
            ]
        , ProgressBar.view details.totalDrvs segments

        -- Stats
        , div [ class "flex justify-around mt3 f7 gray" ]
            [ viewStat "Queued" details.queuedDrvs
            , viewStat "Buildable" details.buildableDrvs
            , viewStat "Building" details.buildingDrvs
            , viewStat "Completed" details.completedDrvs
            , viewStat "Failed" details.failedDrvs
            , viewStat "Transitive" details.transitiveFailureDrvs
            ]
        ]


{-| View a single stat.
-}
viewStat : String -> Int -> Html Msg
viewStat label count =
    div [ class "tc" ]
        [ div [ class "f5 fw6" ] [ text (String.fromInt count) ]
        , div [] [ text label ]
        ]


{-| View derivations table.
-}
viewDrvTable : List JobSetDrv -> Html Msg
viewDrvTable drvs =
    if List.isEmpty drvs then
        div [ class "card pa4 tc" ]
            [ p [ class "gray" ] [ text "No derivations found" ] ]

    else
        div [ class "card" ]
            [ table [ class "table w-100" ]
                [ thead []
                    [ tr []
                        [ th [] [ text "Name" ]
                        , th [] [ text "System" ]
                        , th [] [ text "Status" ]
                        , th [] [ text "Type" ]
                        , th [] [ text "Path" ]
                        ]
                    ]
                , tbody []
                    (List.map viewDrvRow drvs)
                ]
            ]


{-| View a single derivation row.
-}
viewDrvRow : JobSetDrv -> Html Msg
viewDrvRow drv =
    tr []
        [ td []
            [ a
                [ href (Route.toHref (Route.Drv drv.drvPath))
                , class "link blue hover-dark-blue"
                ]
                [ text drv.name ]
            ]
        , td [ class "gray f7" ]
            [ text drv.system ]
        , td []
            [ StatusBadge.view drv.buildState ]
        , td [ class "f7" ]
            [ if drv.isFod then
                text "FOD"

              else if drv.preferLocalBuild then
                text "Local"

              else
                text "-"
            ]
        , td [ class "monospace f7 gray truncate-path" ]
            [ text drv.drvPath ]
        ]


{-| View success/error messages.
-}
viewMessages : JobData -> Html Msg
viewMessages data =
    div []
        [ case data.errorMessage of
            Just err ->
                div [ class "card pa3 mb3 bg-light-red" ]
                    [ div [ class "flex justify-between items-center" ]
                        [ span [ class "dark-red" ] [ text err ]
                        , button
                            [ onClick DismissMessage
                            , class "bn bg-transparent pointer dark-red f4"
                            ]
                            [ text "×" ]
                        ]
                    ]

            Nothing ->
                text ""
        , case data.successMessage of
            Just msg ->
                div [ class "card pa3 mb3 bg-light-green" ]
                    [ div [ class "flex justify-between items-center" ]
                        [ span [ class "dark-green" ] [ text msg ]
                        , button
                            [ onClick DismissMessage
                            , class "bn bg-transparent pointer dark-green f4"
                            ]
                            [ text "×" ]
                        ]
                    ]

            Nothing ->
                text ""
        ]


{-| View maintainers section with list and request button.
-}
viewMaintainersSection : JobData -> Html Msg
viewMaintainersSection data =
    div [ class "card pa3" ]
        [ div [ class "flex justify-between items-center mb3" ]
            [ h2 [ class "f5 fw6 ma0" ]
                [ text "Maintainers" ]
            , viewRequestButton data
            ]
        , if data.loadingMaintainers then
            div [ class "tc pa3 gray" ]
                [ text "Loading maintainers..." ]

          else if List.isEmpty data.maintainers then
            div [ class "tc pa3 gray" ]
                [ text "No maintainers yet. Be the first to request access!" ]

          else
            div [ class "flex gap2 flex-wrap" ]
                (List.map viewMaintainer data.maintainers)
        ]


{-| View a single maintainer badge.
-}
viewMaintainer : MaintainerDetail -> Html Msg
viewMaintainer maintainer =
    div
        [ class "flex items-center gap2 pa2 br2 bg-light-gray"
        , title ("Added " ++ maintainer.addedAt)
        ]
        [ case maintainer.githubAvatarUrl of
            Just avatarUrl ->
                img
                    [ src avatarUrl
                    , class "w2 h2 br-100"
                    ]
                    []

            Nothing ->
                div [ class "w2 h2 br-100 bg-gray" ] []
        , span [ class "f6" ] [ text maintainer.githubUsername ]
        ]


{-| View request maintainer button with different states.
-}
viewRequestButton : JobData -> Html Msg
viewRequestButton data =
    let
        isAuthenticated =
            Auth.isAuthenticated data.authState

        currentUserId =
            data.authState.user
                |> Maybe.map .githubId

        isAlreadyMaintainer =
            currentUserId
                |> Maybe.map (\userId -> List.any (\m -> m.githubUserId == userId) data.maintainers)
                |> Maybe.withDefault False
    in
    if not isAuthenticated then
        a
            [ href (Route.toHref Route.AuthCallback)
            , class "btn-secondary btn-sm"
            ]
            [ text "Log in to request access" ]

    else if isAlreadyMaintainer then
        button
            [ class "btn-secondary btn-sm"
            , disabled True
            ]
            [ text "You are a maintainer" ]

    else if data.requestingAccess then
        button
            [ class "btn-primary btn-sm"
            , disabled True
            ]
            [ text "Requesting..." ]

    else
        button
            [ onClick RequestMaintainerAccess
            , class "btn-primary btn-sm"
            ]
            [ text "Request Maintainer Access" ]



{- HELPER FUNCTIONS FOR REAL-TIME UPDATES -}


{-| Update a single derivation's build state in the list.
-}
updateDrvState : String -> BS.DrvBuildState -> List JobSetDrv -> List JobSetDrv
updateDrvState drvPath newState drvs =
    List.map
        (\drv ->
            if drv.drvPath == drvPath then
                { drv | buildState = newState }

            else
                drv
        )
        drvs


{-| Recalculate job statistics based on current derivation states.
-}
recalculateStats : List JobSetDrv -> JobSetDetails -> JobSetDetails
recalculateStats drvs details =
    let
        counts =
            List.foldl countDrvState initialCounts drvs
    in
    { details
        | queuedDrvs = counts.queued
        , buildableDrvs = counts.buildable
        , buildingDrvs = counts.building
        , completedDrvs = counts.completed
        , failedDrvs = counts.failed
        , transitiveFailureDrvs = counts.transitive
    }


type alias DrvCounts =
    { queued : Int
    , buildable : Int
    , building : Int
    , completed : Int
    , failed : Int
    , transitive : Int
    }


initialCounts : DrvCounts
initialCounts =
    { queued = 0
    , buildable = 0
    , building = 0
    , completed = 0
    , failed = 0
    , transitive = 0
    }


countDrvState : JobSetDrv -> DrvCounts -> DrvCounts
countDrvState drv counts =
    case drv.buildState of
        BS.Queued ->
            { counts | queued = counts.queued + 1 }

        BS.Buildable ->
            { counts | buildable = counts.buildable + 1 }

        BS.Building ->
            { counts | building = counts.building + 1 }

        BS.Completed BS.Success ->
            { counts | completed = counts.completed + 1 }

        BS.Completed BS.Failure ->
            { counts | failed = counts.failed + 1 }

        BS.TransitiveFailure ->
            { counts | transitive = counts.transitive + 1 }

        BS.FailedRetry ->
            { counts | queued = counts.queued + 1 }

        BS.Interrupted _ ->
            { counts | failed = counts.failed + 1 }

        BS.Blocked ->
            counts


{-| Request maintainer access for an attr path (authenticated API call).
-}
requestMaintainerAccess : String -> String -> String -> Cmd Msg
requestMaintainerAccess apiBaseUrl authToken attrPath =
    Http.request
        { method = "POST"
        , headers = [ Http.header "Authorization" ("Bearer " ++ authToken) ]
        , url = apiBaseUrl ++ "/attr-paths/" ++ attrPath ++ "/request-maintainer"
        , body = Http.emptyBody
        , expect = Http.expectWhatever MaintainerRequestCompleted
        , timeout = Nothing
        , tracker = Nothing
        }


{-| Convert HTTP error to user-friendly string.
-}
httpErrorToString : Http.Error -> String
httpErrorToString error =
    case error of
        Http.BadUrl _ ->
            "Invalid URL"

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus code ->
            if code == 401 then
                "Not authenticated"

            else if code == 403 then
                "Not authorized"

            else if code == 409 then
                "You already have a pending request or are already a maintainer"

            else
                "Server error: " ++ String.fromInt code

        Http.BadBody message ->
            "Invalid response: " ++ message

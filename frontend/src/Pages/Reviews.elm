module Pages.Reviews exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Pull Request Review Portal page.

Displays all open PRs with build statistics, sorted by CI status,
changed drvs count, and lines changed.

-}

import Api.Api as Api
import Components.ErrorView as ErrorView
import Components.Loader as Loader
import Components.StatusBadge as StatusBadge
import Html exposing (Html, a, button, div, h1, h2, input, label, p, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (checked, class, href, type_)
import Html.Events exposing (onCheck, onClick)
import Http
import Models.PullRequest exposing (PullRequest)
import Route


{-| Page model with PR list data.
-}
type Model
    = Loading String
    | Loaded LoadedData
    | Failed Http.Error


{-| Loaded data with PRs and sort settings.
-}
type alias LoadedData =
    { prs : List PullRequest
    , apiBaseUrl : String
    , sortBy : SortBy
    , hideFailingGates : Bool
    }


{-| Sort options for PRs.
-}
type SortBy
    = ByChangedDrvs
    | ByLinesChanged -- Not yet implemented, needs GitHub API integration
    | ByLastUpdated


{-| Page messages.
-}
type Msg
    = GotPullRequests (Result Http.Error (List PullRequest))
    | ChangeSortBy SortBy
    | ToggleHideFailingGates Bool


{-| Initialize the page.
-}
init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( Loading apiBaseUrl
    , Api.getPullRequests apiBaseUrl GotPullRequests
    )


{-| Update the page.
-}
update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotPullRequests result ->
            case ( result, model ) of
                ( Ok prs, Loading apiBaseUrl ) ->
                    ( Loaded
                        { prs = prs
                        , apiBaseUrl = apiBaseUrl
                        , sortBy = ByChangedDrvs
                        , hideFailingGates = False
                        }
                    , Cmd.none
                    )

                ( Ok prs, Loaded data ) ->
                    ( Loaded { data | prs = prs }
                    , Cmd.none
                    )

                ( Ok _, _ ) ->
                    ( model, Cmd.none )

                ( Err err, _ ) ->
                    ( Failed err, Cmd.none )

        ChangeSortBy sortBy ->
            case model of
                Loaded data ->
                    ( Loaded { data | sortBy = sortBy }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        ToggleHideFailingGates hide ->
            case model of
                Loaded data ->
                    ( Loaded { data | hideFailingGates = hide }, Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| Render the page.
-}
view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            Loader.view

        Failed err ->
            ErrorView.viewHttp err

        Loaded data ->
            viewLoaded data


{-| Render the loaded page with PR list.
-}
viewLoaded : LoadedData -> Html Msg
viewLoaded data =
    let
        filteredAndSortedPRs =
            data.prs
                |> filterPRs data.hideFailingGates
                |> sortPRs data.sortBy
    in
    div [ class "container mx-auto px-4 py-8" ]
        [ h1 [ class "text-3xl font-bold mb-6" ] [ text "Pull Request Review Portal" ]
        , viewControls data.sortBy data.hideFailingGates
        , if List.isEmpty filteredAndSortedPRs then
            p [ class "text-gray-500 mt-8" ] [ text "No open pull requests found." ]

          else
            viewPRTable filteredAndSortedPRs
        ]


{-| View sorting and filtering controls.
-}
viewControls : SortBy -> Bool -> Html Msg
viewControls currentSort hideFailingGates =
    div [ class "mb-6 p-4 bg-gray-50 rounded-lg" ]
        [ div [ class "flex flex-wrap items-center gap-4" ]
            [ div [ class "flex items-center gap-2" ]
                [ label [ class "font-semibold" ] [ text "Sort by:" ]
                , viewSortButton "Changed Drvs" ByChangedDrvs (currentSort == ByChangedDrvs)
                , viewSortButton "Last Updated" ByLastUpdated (currentSort == ByLastUpdated)
                ]
            , div [ class "flex items-center gap-2" ]
                [ input
                    [ type_ "checkbox"
                    , checked hideFailingGates
                    , onCheck ToggleHideFailingGates
                    , class "rounded"
                    ]
                    []
                , label [ class "text-sm" ] [ text "Hide PRs with failing CI gates" ]
                ]
            ]
        ]


{-| View a sort button.
-}
viewSortButton : String -> SortBy -> Bool -> Html Msg
viewSortButton label sortBy isActive =
    button
        [ onClick (ChangeSortBy sortBy)
        , class
            (if isActive then
                "px-3 py-1 rounded bg-blue-600 text-white"

             else
                "px-3 py-1 rounded bg-gray-200 hover:bg-gray-300"
            )
        ]
        [ text label ]


{-| View the PR table.
-}
viewPRTable : List PullRequest -> Html Msg
viewPRTable prs =
    table [ class "min-w-full bg-white border border-gray-200" ]
        [ thead [ class "bg-gray-100" ]
            [ tr []
                [ th [ class "px-4 py-2 text-left" ] [ text "Repository" ]
                , th [ class "px-4 py-2 text-left" ] [ text "PR" ]
                , th [ class "px-4 py-2 text-left" ] [ text "Title" ]
                , th [ class "px-4 py-2 text-left" ] [ text "Author" ]
                , th [ class "px-4 py-2 text-center" ] [ text "CI Status" ]
                , th [ class "px-4 py-2 text-right" ] [ text "Changed Drvs" ]
                , th [ class "px-4 py-2 text-right" ] [ text "Total Drvs" ]
                ]
            ]
        , tbody [] (List.map viewPRRow prs)
        ]


{-| View a single PR row.
-}
viewPRRow : PullRequest -> Html Msg
viewPRRow pr =
    let
        prUrl =
            Route.toHref (Route.PullRequest pr.owner pr.repoName pr.prNumber)

        ciPassed =
            pr.completedFailureDrvs == 0 && pr.failedRetryDrvs == 0 && pr.totalDrvs > 0

        ciInProgress =
            pr.totalDrvs > 0 && (pr.completedSuccessDrvs + pr.completedFailureDrvs) < pr.totalDrvs

        statusBadge =
            if ciPassed then
                span [ class "px-2 py-1 rounded bg-green-100 text-green-800 text-sm" ] [ text "✓ Passed" ]

            else if ciInProgress then
                span [ class "px-2 py-1 rounded bg-yellow-100 text-yellow-800 text-sm" ] [ text "⋯ In Progress" ]

            else if pr.totalDrvs == 0 then
                span [ class "px-2 py-1 rounded bg-gray-100 text-gray-600 text-sm" ] [ text "No builds" ]

            else
                span [ class "px-2 py-1 rounded bg-red-100 text-red-800 text-sm" ] [ text "✗ Failed" ]
    in
    tr [ class "border-t border-gray-200 hover:bg-gray-50" ]
        [ td [ class "px-4 py-3" ]
            [ text (pr.owner ++ "/" ++ pr.repoName) ]
        , td [ class "px-4 py-3" ]
            [ a [ href prUrl, class "text-blue-600 hover:underline" ]
                [ text ("#" ++ String.fromInt pr.prNumber) ]
            ]
        , td [ class "px-4 py-3 max-w-md truncate" ]
            [ a [ href prUrl, class "hover:underline" ]
                [ text pr.title ]
            ]
        , td [ class "px-4 py-3 text-sm text-gray-600" ]
            [ text pr.author ]
        , td [ class "px-4 py-3 text-center" ]
            [ statusBadge ]
        , td [ class "px-4 py-3 text-right font-mono text-sm" ]
            [ text (String.fromInt pr.changedDrvs) ]
        , td [ class "px-4 py-3 text-right font-mono text-sm" ]
            [ text (String.fromInt pr.totalDrvs) ]
        ]


{-| Filter PRs based on settings.
-}
filterPRs : Bool -> List PullRequest -> List PullRequest
filterPRs hideFailingGates prs =
    if hideFailingGates then
        prs
            |> List.filter
                (\pr ->
                    -- Only show PRs with all CI gates passing
                    pr.completedFailureDrvs == 0 && pr.failedRetryDrvs == 0
                )

    else
        prs


{-| Sort PRs based on the selected sort option.
-}
sortPRs : SortBy -> List PullRequest -> List PullRequest
sortPRs sortBy prs =
    let
        -- Primary sort: CI gates passing (all green first)
        byCI pr =
            if pr.completedFailureDrvs == 0 && pr.failedRetryDrvs == 0 && pr.totalDrvs > 0 then
                0

            else
                1

        -- Secondary sort based on selection
        secondarySort =
            case sortBy of
                ByChangedDrvs ->
                    \pr -> ( byCI pr, pr.changedDrvs, pr.updatedAt )

                ByLastUpdated ->
                    \pr -> ( byCI pr, pr.updatedAt, pr.changedDrvs )

                ByLinesChanged ->
                    -- Not implemented yet, fall back to changed drvs
                    \pr -> ( byCI pr, pr.changedDrvs, pr.updatedAt )
    in
    prs
        |> List.sortBy (\pr -> ( byCI pr, pr.changedDrvs ))

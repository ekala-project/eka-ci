module Pages.Reviews exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Reviews: open pull requests with their CI results.

Pull requests can be filtered by CI status and text, and sorted by how many
derivations they change or by when they were last updated. Within a sort,
passing pull requests come first.

-}

import Api.Api as Api
import Components.BuildProgress as Progress
import Components.Ui as Ui
import Html exposing (Html, a, div, input, label, p, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (attribute, class, href, id, placeholder, scope, title, type_, value)
import Html.Events exposing (onInput)
import Http
import Models.PullRequest exposing (PullRequest)
import Route


type alias Model =
    { apiBaseUrl : String
    , prs : Remote (List PullRequest)
    , sortBy : SortBy
    , status : Maybe Status
    , search : String
    }


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


type SortBy
    = FewestChanges
    | RecentlyUpdated


{-| CI status of a pull request, in the order reviewers care about.
-}
type Status
    = Passed
    | InProgress
    | Failing
    | NoBuilds


type Msg
    = GotPullRequests (Result Http.Error (List PullRequest))
    | SetSort SortBy
    | SetStatus (Maybe Status)
    | SetSearch String


init : String -> ( Model, Cmd Msg )
init apiBaseUrl =
    ( { apiBaseUrl = apiBaseUrl
      , prs = Loading
      , sortBy = FewestChanges
      , status = Nothing
      , search = ""
      }
    , Api.getPullRequests apiBaseUrl GotPullRequests
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotPullRequests (Ok prs) ->
            ( { model | prs = Ready prs }, Cmd.none )

        GotPullRequests (Err error) ->
            ( { model | prs = Failed error }, Cmd.none )

        SetSort sortBy ->
            ( { model | sortBy = sortBy }, Cmd.none )

        SetStatus status ->
            ( { model | status = status }, Cmd.none )

        SetSearch search ->
            ( { model | search = search }, Cmd.none )



-- STATUS AND SORTING


statusOf : PullRequest -> Status
statusOf pr =
    case Progress.overall (Progress.fromPullRequest pr) of
        ( Ui.Good, _ ) ->
            Passed

        ( Ui.Critical, _ ) ->
            Failing

        ( Ui.Neutral, _ ) ->
            NoBuilds

        _ ->
            InProgress


statusRank : Status -> Int
statusRank status =
    case status of
        Passed ->
            0

        InProgress ->
            1

        Failing ->
            2

        NoBuilds ->
            3


statusLabel : Status -> String
statusLabel status =
    case status of
        Passed ->
            "Passed"

        InProgress ->
            "In progress"

        Failing ->
            "Failing"

        NoBuilds ->
            "No builds"


statusTone : Status -> Ui.Tone
statusTone status =
    case status of
        Passed ->
            Ui.Good

        InProgress ->
            Ui.Info

        Failing ->
            Ui.Critical

        NoBuilds ->
            Ui.Neutral


{-| Passing first, then by the chosen key. `List.sortWith` is stable, so
equal keys keep the server's order.
-}
sortPRs : SortBy -> List PullRequest -> List PullRequest
sortPRs sortBy =
    List.sortWith
        (\a b ->
            case compare (statusRank (statusOf a)) (statusRank (statusOf b)) of
                EQ ->
                    case sortBy of
                        FewestChanges ->
                            compare a.changedDrvs b.changedDrvs

                        RecentlyUpdated ->
                            -- ISO-8601 timestamps sort lexically; newest first.
                            compare b.updatedAt a.updatedAt

                order ->
                    order
        )


matches : Maybe Status -> String -> PullRequest -> Bool
matches status search pr =
    let
        needle =
            String.toLower (String.trim search)

        haystack =
            String.toLower (String.join " " [ pr.title, pr.author, pr.owner ++ "/" ++ pr.repoName, "#" ++ String.fromInt pr.prNumber ])
    in
    (status == Nothing || status == Just (statusOf pr))
        && (String.isEmpty needle || String.contains needle haystack)



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ Ui.simpleHead "Reviews" "Open pull requests and their CI results. Passing pull requests are listed first."
        , case model.prs of
            Loading ->
                Ui.loading "Loading pull requests…"

            Failed error ->
                Ui.httpError "Could not load pull requests." error

            Ready [] ->
                Ui.emptyState "No open pull requests" "Pull requests on installed repositories will show up here once EkaCI evaluates them."

            Ready prs ->
                viewLoaded model prs
        ]


viewLoaded : Model -> List PullRequest -> Html Msg
viewLoaded model prs =
    let
        shown =
            prs
                |> List.filter (matches model.status model.search)
                |> sortPRs model.sortBy
    in
    div []
        [ statusChips model.status prs
        , div [ class "toolbar" ]
            [ label [ class "field grow" ]
                [ text "Filter"
                , input
                    [ type_ "search"
                    , placeholder "title, author, repository or #number"
                    , value model.search
                    , onInput SetSearch
                    , attribute "aria-describedby" "pr-count"
                    ]
                    []
                ]
            , sortChips model.sortBy
            ]
        , p [ id "pr-count", class "small muted", attribute "aria-live" "polite" ]
            [ text (countText (List.length shown) (List.length prs)) ]
        , if List.isEmpty shown then
            Ui.emptyState "No matching pull requests" "Try a different status or filter."

          else
            Ui.tableWrap (prTable shown)
        ]


statusChips : Maybe Status -> List PullRequest -> Html Msg
statusChips current prs =
    let
        countFor status =
            List.length (List.filter (\pr -> statusOf pr == status) prs)

        statusChip status =
            Ui.chip
                { label = statusLabel status
                , count = Just (countFor status)
                , tone = Just (statusTone status)
                , pressed = current == Just status
                , onPress = SetStatus (Just status)
                }
    in
    div [ class "chips", attribute "role" "group", attribute "aria-label" "Filter by CI status" ]
        (Ui.chip { label = "All", count = Just (List.length prs), tone = Nothing, pressed = current == Nothing, onPress = SetStatus Nothing }
            :: List.map statusChip (List.filter (\s -> countFor s > 0) [ Passed, InProgress, Failing, NoBuilds ])
        )


sortChips : SortBy -> Html Msg
sortChips current =
    div [ class "chips", attribute "role" "group", attribute "aria-label" "Sort" ]
        [ span [ class "small muted" ] [ text "Sort:" ]
        , Ui.chip { label = "Fewest changes", count = Nothing, tone = Nothing, pressed = current == FewestChanges, onPress = SetSort FewestChanges }
        , Ui.chip { label = "Recently updated", count = Nothing, tone = Nothing, pressed = current == RecentlyUpdated, onPress = SetSort RecentlyUpdated }
        ]


countText : Int -> Int -> String
countText shown total =
    if shown == total then
        String.fromInt total
            ++ (if total == 1 then
                    " pull request"

                else
                    " pull requests"
               )

    else
        String.fromInt shown ++ " of " ++ String.fromInt total ++ " pull requests"


prTable : List PullRequest -> Html Msg
prTable prs =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Pull request" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col" ] [ text "Progress" ]
                , th [ scope "col", class "num hide-sm" ] [ text "Changed" ]
                , th [ scope "col", class "hide-sm" ] [ text "Updated" ]
                ]
            ]
        , tbody [] (List.map prRow prs)
        ]


prRow : PullRequest -> Html Msg
prRow pr =
    let
        counts =
            Progress.fromPullRequest pr
    in
    tr []
        [ td [] [ prTitle pr, prSubline pr ]
        , td [] [ Ui.badge (statusTone (statusOf pr)) (statusLabel (statusOf pr)) ]
        , td [ class "progress-cell" ]
            [ if counts.total == 0 then
                span [ class "small muted" ] [ text "Not evaluated yet" ]

              else
                Progress.cell counts
            ]
        , td [ class "num hide-sm", title (changedTitle pr) ]
            [ text (String.fromInt pr.changedDrvs)
            , if pr.newDrvs > 0 then
                span [ class "cell-sub" ] [ text (String.fromInt pr.newDrvs ++ " new") ]

              else
                text ""
            ]
        , td [ class "nowrap small hide-sm" ] [ Ui.timestamp pr.updatedAt ]
        ]


{-| The title opens the PR's job when EkaCI has one; otherwise GitHub.
-}
prTitle : PullRequest -> Html Msg
prTitle pr =
    case pr.jobsetId of
        Just jobsetId ->
            a [ class "cell-main", href (Route.toHref (Route.Job jobsetId)) ] [ text pr.title ]

        Nothing ->
            a [ class "cell-main", href (gitHubUrl pr), Html.Attributes.target "_blank", Html.Attributes.rel "noopener noreferrer" ]
                [ text pr.title ]


prSubline : PullRequest -> Html Msg
prSubline pr =
    span [ class "cell-sub" ]
        [ a [ href (Route.toHref (Route.Repository pr.owner pr.repoName)) ] [ text (pr.owner ++ "/" ++ pr.repoName) ]
        , text " "
        , Ui.externalLink (gitHubUrl pr) [ text ("#" ++ String.fromInt pr.prNumber) ]
        , text (" by " ++ pr.author)
        ]


{-| The forge's own URL for the PR (github.com, GHE, ...). The server only
learns it the next time it sees the PR, so older rows fall back to a
github.com URL; ekaci has no pull request page of its own.
-}
gitHubUrl : PullRequest -> String
gitHubUrl pr =
    Maybe.withDefault (fallbackUrl pr) pr.htmlUrl


fallbackUrl : PullRequest -> String
fallbackUrl pr =
    "https://github.com/" ++ pr.owner ++ "/" ++ pr.repoName ++ "/pull/" ++ String.fromInt pr.prNumber


changedTitle : PullRequest -> String
changedTitle pr =
    String.fromInt pr.changedDrvs ++ " changed derivations, " ++ String.fromInt pr.newDrvs ++ " new"

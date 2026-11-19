module Pages.Repository exposing
    ( Model
    , Msg
    , init
    , update
    , view
    )

{-| Repository details page.

Shows a repository's recent job evaluations with their build progress and
package changes.

-}

import Api.Api as Api
import Components.BuildProgress as Progress
import Components.Ui as Ui
import Html exposing (Html, a, div, h1, span, table, tbody, td, text, th, thead, tr)
import Html.Attributes exposing (attribute, class, href, scope, title)
import Http
import Models.Repository exposing (Repository, RepositoryJobSet, SortOrder(..))
import Route


type Remote a
    = Loading
    | Ready a
    | Failed Http.Error


{-| Page model. The repository and its jobsets load independently, so
whichever response arrives first is kept.
-}
type alias Model =
    { owner : String
    , repoName : String
    , repository : Remote Repository
    , jobsets : Remote (List RepositoryJobSet)
    , sortOrder : SortOrder
    }


type Msg
    = GotRepository (Result Http.Error Repository)
    | GotJobsets (Result Http.Error (List RepositoryJobSet))
    | SetSortOrder SortOrder


jobsetLimit : Int
jobsetLimit =
    50


init : String -> String -> String -> ( Model, Cmd Msg )
init apiBaseUrl owner repoName =
    ( { owner = owner
      , repoName = repoName
      , repository = Loading
      , jobsets = Loading
      , sortOrder = DateDesc
      }
    , Cmd.batch
        [ Api.getRepository apiBaseUrl owner repoName GotRepository
        , Api.getRepositoryJobsets apiBaseUrl owner repoName jobsetLimit True GotJobsets
        ]
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotRepository result ->
            ( { model | repository = toRemote result }, Cmd.none )

        GotJobsets result ->
            ( { model | jobsets = toRemote result }, Cmd.none )

        SetSortOrder order ->
            ( { model | sortOrder = order }, Cmd.none )


toRemote : Result Http.Error a -> Remote a
toRemote result =
    case result of
        Ok value ->
            Ready value

        Err error ->
            Failed error



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ Ui.pageHead
            { crumbs = [ ( "Repositories", Route.toHref Route.Home ) ]
            , title = [ h1 [] [ text (model.owner ++ "/" ++ model.repoName) ] ]
            , lede = [ text "Recent evaluations of this repository's jobs." ]
            , actions = []
            }
        , case model.repository of
            Failed error ->
                Ui.httpError "Could not load this repository." error

            _ ->
                viewJobsets model
        ]


viewJobsets : Model -> Html Msg
viewJobsets model =
    case model.jobsets of
        Loading ->
            Ui.loading "Loading recent evaluations…"

        Failed error ->
            Ui.httpError "Could not load recent evaluations." error

        Ready [] ->
            Ui.emptyState "No evaluations yet"
                "Jobs appear here once EkaCI evaluates a push or pull request."

        Ready jobsets ->
            Ui.panel "Recent evaluations"
                [ sortChips model.sortOrder ]
                [ Ui.tableWrap (jobsetTable (sorted model.sortOrder jobsets))
                , if List.length jobsets >= jobsetLimit then
                    Html.p [ class "small muted" ]
                        [ text ("Showing the " ++ String.fromInt jobsetLimit ++ " most recent evaluations.") ]

                  else
                    text ""
                ]


{-| The API returns newest first.
-}
sorted : SortOrder -> List RepositoryJobSet -> List RepositoryJobSet
sorted order jobsets =
    case order of
        DateDesc ->
            jobsets

        DateAsc ->
            List.reverse jobsets


sortChips : SortOrder -> Html Msg
sortChips order =
    div [ class "chips", attribute "role" "group", attribute "aria-label" "Sort order" ]
        [ Ui.chip { label = "Newest first", count = Nothing, tone = Nothing, pressed = order == DateDesc, onPress = SetSortOrder DateDesc }
        , Ui.chip { label = "Oldest first", count = Nothing, tone = Nothing, pressed = order == DateAsc, onPress = SetSortOrder DateAsc }
        ]


jobsetTable : List RepositoryJobSet -> Html Msg
jobsetTable jobsets =
    table [ class "table" ]
        [ thead []
            [ tr []
                [ th [ scope "col" ] [ text "Job" ]
                , th [ scope "col" ] [ text "Commit" ]
                , th [ scope "col" ] [ text "Status" ]
                , th [ scope "col" ] [ text "Progress" ]
                , th [ scope "col", class "hide-sm" ] [ text "Package changes" ]
                ]
            ]
        , tbody [] (List.map jobsetRow jobsets)
        ]


jobsetRow : RepositoryJobSet -> Html Msg
jobsetRow jobset =
    let
        counts =
            Progress.fromJobSet jobset
    in
    tr []
        [ td [] [ a [ class "cell-main", href (Route.toHref (Route.Job jobset.jobsetId)) ] [ text jobset.jobName ] ]
        , td []
            [ a [ class "mono", href (Route.toHref (Route.Commit jobset.sha)), title jobset.sha ]
                [ text (Ui.shortSha jobset.sha) ]
            ]
        , td [] [ Progress.overallBadge counts ]
        , td [ class "progress-cell" ] [ Progress.cell counts ]
        , td [ class "hide-sm" ] [ changeSummary jobset ]
        ]


changeSummary : RepositoryJobSet -> Html msg
changeSummary jobset =
    let
        parts =
            List.filterMap identity
                [ changePart jobset.newJobs ("+" ++ String.fromInt jobset.newJobs ++ " new") "tone-text-good"
                , changePart jobset.changedJobs (String.fromInt jobset.changedJobs ++ " changed") ""
                , changePart jobset.removedJobs ("−" ++ String.fromInt jobset.removedJobs ++ " removed") "tone-text-serious"
                ]
    in
    if List.isEmpty parts then
        span [ class "muted" ] [ text "None" ]

    else
        span [ class "small" ] (List.intersperse (text " · ") parts)


changePart : Int -> String -> String -> Maybe (Html msg)
changePart n label classes =
    if n > 0 then
        Just (span [ class classes ] [ text label ])

    else
        Nothing

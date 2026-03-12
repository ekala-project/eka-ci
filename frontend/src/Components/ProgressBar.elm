module Components.ProgressBar exposing (view)

{-| Progress bar component for visualizing job progress.

Displays a multi-segment progress bar showing the distribution of
derivations in different build states.

-}

import Html exposing (Html, div, text)
import Html.Attributes exposing (class, style)


{-| Configuration for a progress bar segment.
-}
type alias Segment =
    { label : String
    , count : Int
    , cssClass : String
    }


{-| View a progress bar with multiple segments.

    view 100 [ { label = "Success", count = 75, cssClass = "progress-segment-success" }
             , { label = "Failed", count = 25, cssClass = "progress-segment-failure" }
             ]
    --> Progress bar showing 75% success, 25% failure

-}
view : Int -> List Segment -> Html msg
view total segments =
    if total == 0 then
        div [ class "progress-container" ]
            [ div [ class "tc pv2 f6 gray" ]
                [ text "No builds" ]
            ]

    else
        div [ class "progress-container" ]
            [ div [ class "progress-bar" ]
                (List.map (viewSegment total) segments)
            ]


{-| View a single segment of the progress bar.
-}
viewSegment : Int -> Segment -> Html msg
viewSegment total segment =
    let
        percentage =
            toFloat segment.count / toFloat total * 100

        widthStyle =
            style "width" (String.fromFloat percentage ++ "%")

        displayText =
            if percentage > 5 then
                -- Only show text if segment is wide enough
                String.fromInt segment.count

            else
                ""
    in
    if segment.count > 0 then
        div
            [ class ("progress-segment " ++ segment.cssClass)
            , widthStyle
            ]
            [ text displayText ]

    else
        text ""


{-| Create segments for a job's build statistics.
-}
fromJobStats :
    { completed : Int
    , failed : Int
    , building : Int
    , queued : Int
    }
    -> List Segment
fromJobStats stats =
    [ { label = "Completed"
      , count = stats.completed
      , cssClass = "progress-segment-success"
      }
    , { label = "Failed"
      , count = stats.failed
      , cssClass = "progress-segment-failure"
      }
    , { label = "Building"
      , count = stats.building
      , cssClass = "progress-segment-building"
      }
    , { label = "Queued"
      , count = stats.queued
      , cssClass = "progress-segment-queued"
      }
    ]

module Models.Repository exposing (Repository)

{-| Repository model matching the backend API response.

Represents a GitHub repository that has the EkaCI app installed.

-}


{-| A GitHub repository where the app is installed.
-}
type alias Repository =
    { owner : String
    , repoName : String
    , installationId : Int
    }

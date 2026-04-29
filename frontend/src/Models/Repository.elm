module Models.Repository exposing (Repository, RepositoryJobSet, SortOrder(..))

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


{-| Summary of a jobset for repository listing with build stats and change summary.
-}
type alias RepositoryJobSet =
    { jobsetId : Int
    , jobName : String
    , sha : String
    , totalDrvs : Int
    , queuedDrvs : Int
    , buildableDrvs : Int
    , buildingDrvs : Int
    , failedRetryDrvs : Int
    , completedSuccessDrvs : Int
    , completedFailureDrvs : Int
    , transitiveFailureDrvs : Int
    , blockedDrvs : Int
    , interruptedDrvs : Int
    , newJobs : Int
    , changedJobs : Int
    , removedJobs : Int
    }


{-| Sort order for jobset list
-}
type SortOrder
    = DateDesc
    | DateAsc

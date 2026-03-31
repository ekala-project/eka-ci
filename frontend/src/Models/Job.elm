module Models.Job exposing
    ( CommitJob
    , JobSetDetails
    , JobSetDrv
    )

{-| Job and jobset models matching the backend API responses.
-}

import Models.BuildState exposing (DrvBuildState)
import Time


{-| A job associated with a commit (from GET /v1/commits/{sha}/jobs).
-}
type alias CommitJob =
    { jobsetId : Int
    , jobName : String
    , sha : String
    , owner : String
    , repoName : String
    }


{-| Detailed information about a jobset (from GET /v1/jobs/{jobset\_id}).
-}
type alias JobSetDetails =
    { jobsetId : Int
    , sha : String
    , jobName : String
    , owner : String
    , repoName : String
    , totalDrvs : Int
    , queuedDrvs : Int
    , buildableDrvs : Int
    , buildingDrvs : Int
    , completedDrvs : Int
    , failedDrvs : Int
    , transitiveFailureDrvs : Int
    }


{-| A derivation in a jobset (from GET /v1/jobs/{jobset\_id}/drvs).
-}
type alias JobSetDrv =
    { drvPath : String
    , name : String
    , system : String
    , buildState : DrvBuildState
    , isFod : Bool
    , preferLocalBuild : Bool
    }

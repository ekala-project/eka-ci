module Api.Api exposing
    ( getRepositories
    , getRepository
    , getCommitJobs
    , getJobSetDetails
    , getJobSetDrvs
    , getDrvDetails
    , getDrvDependencies
    )

{-| HTTP API client for the EkaCI backend.

All functions take a message constructor that will be called with the Result.

-}

import Api.Decoder as Decoder
import Http
import Models.Derivation exposing (DrvDependency, DrvDetails)
import Models.Job exposing (CommitJob, JobSetDetails, JobSetDrv)
import Models.Repository exposing (Repository)
import Url.Builder as UB


{-| Base URL for the API.
-}
apiBase : String
apiBase =
    "/v1"


{-| Get all repositories where the app is installed.

    getRepositories GotRepositories

-}
getRepositories : (Result Http.Error (List Repository) -> msg) -> Cmd msg
getRepositories toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "repositories" ] []
        , expect = Http.expectJson toMsg Decoder.repositoryList
        }


{-| Get details about a specific repository.

    getRepository "owner" "repo" GotRepository

-}
getRepository : String -> String -> (Result Http.Error Repository -> msg) -> Cmd msg
getRepository owner repoName toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "repositories", owner, repoName ] []
        , expect = Http.expectJson toMsg Decoder.repository
        }


{-| Get all jobs for a commit.

    getCommitJobs "abc123..." GotCommitJobs

-}
getCommitJobs : String -> (Result Http.Error (List CommitJob) -> msg) -> Cmd msg
getCommitJobs sha toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "commits", sha, "jobs" ] []
        , expect = Http.expectJson toMsg Decoder.commitJobList
        }


{-| Get details about a jobset.

    getJobSetDetails 123 GotJobSetDetails

-}
getJobSetDetails : Int -> (Result Http.Error JobSetDetails -> msg) -> Cmd msg
getJobSetDetails jobsetId toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "jobs", String.fromInt jobsetId ] []
        , expect = Http.expectJson toMsg Decoder.jobSetDetails
        }


{-| Get all derivations in a jobset.

    getJobSetDrvs 123 GotJobSetDrvs

-}
getJobSetDrvs : Int -> (Result Http.Error (List JobSetDrv) -> msg) -> Cmd msg
getJobSetDrvs jobsetId toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "jobs", String.fromInt jobsetId, "drvs" ] []
        , expect = Http.expectJson toMsg Decoder.jobSetDrvList
        }


{-| Get details about a derivation.

    getDrvDetails "/nix/store/..." GotDrvDetails

-}
getDrvDetails : String -> (Result Http.Error DrvDetails -> msg) -> Cmd msg
getDrvDetails drvPath toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "drvs", drvPath ] []
        , expect = Http.expectJson toMsg Decoder.drvDetails
        }


{-| Get dependencies of a derivation.

    getDrvDependencies "/nix/store/..." GotDrvDependencies

-}
getDrvDependencies : String -> (Result Http.Error (List DrvDependency) -> msg) -> Cmd msg
getDrvDependencies drvPath toMsg =
    Http.get
        { url = UB.absolute [ apiBase, "drvs", drvPath, "dependencies" ] []
        , expect = Http.expectJson toMsg Decoder.drvDependencyList
        }

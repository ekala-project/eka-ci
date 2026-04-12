module Api.Api exposing
    ( ActiveBuildsResponse
    , getActiveBuilds
    , getCommitJobs
    , getDrvDependencies
    , getDrvDetails
    , getJobSetDetails
    , getJobSetDrvs
    , getPRGitHubMetadata
    , getPullRequest
    , getPullRequests
    , getRepositories
    , getRepository
    )

{-| HTTP API client for the EkaCI backend.

All functions take a message constructor that will be called with the Result.

-}

import Api.Decoder as Decoder
import Http
import Models.Derivation exposing (DrvDependency, DrvDetails)
import Models.Job exposing (BuildingDrv, CommitJob, JobSetDetails, JobSetDrv)
import Models.PullRequest exposing (GitHubMetadata, PullRequest)
import Models.Repository exposing (Repository)
import Url.Builder as UB


{-| Active builds response containing jobs and their building drvs.
-}
type alias ActiveBuildsResponse =
    { jobs : List JobSetDetails
    , buildingDrvs : List BuildingDrv
    }


{-| Get all repositories where the app is installed.

    getRepositories apiBaseUrl GotRepositories

-}
getRepositories : String -> (Result Http.Error (List Repository) -> msg) -> Cmd msg
getRepositories apiBaseUrl toMsg =
    Http.get
        { url = apiBaseUrl ++ "/repositories"
        , expect = Http.expectJson toMsg Decoder.repositoryList
        }


{-| Get details about a specific repository.

    getRepository apiBaseUrl "owner" "repo" GotRepository

-}
getRepository : String -> String -> String -> (Result Http.Error Repository -> msg) -> Cmd msg
getRepository apiBaseUrl owner repoName toMsg =
    Http.get
        { url = apiBaseUrl ++ "/repositories/" ++ owner ++ "/" ++ repoName
        , expect = Http.expectJson toMsg Decoder.repository
        }


{-| Get all jobs for a commit.

    getCommitJobs apiBaseUrl "abc123..." GotCommitJobs

-}
getCommitJobs : String -> String -> (Result Http.Error (List CommitJob) -> msg) -> Cmd msg
getCommitJobs apiBaseUrl sha toMsg =
    Http.get
        { url = apiBaseUrl ++ "/commits/" ++ sha ++ "/jobs"
        , expect = Http.expectJson toMsg Decoder.commitJobList
        }


{-| Get details about a jobset.

    getJobSetDetails apiBaseUrl 123 GotJobSetDetails

-}
getJobSetDetails : String -> Int -> (Result Http.Error JobSetDetails -> msg) -> Cmd msg
getJobSetDetails apiBaseUrl jobsetId toMsg =
    Http.get
        { url = apiBaseUrl ++ "/jobs/" ++ String.fromInt jobsetId
        , expect = Http.expectJson toMsg Decoder.jobSetDetails
        }


{-| Get all derivations in a jobset.

    getJobSetDrvs apiBaseUrl 123 GotJobSetDrvs

-}
getJobSetDrvs : String -> Int -> (Result Http.Error (List JobSetDrv) -> msg) -> Cmd msg
getJobSetDrvs apiBaseUrl jobsetId toMsg =
    Http.get
        { url = apiBaseUrl ++ "/jobs/" ++ String.fromInt jobsetId ++ "/drvs"
        , expect = Http.expectJson toMsg Decoder.jobSetDrvList
        }


{-| Get details about a derivation.

    getDrvDetails apiBaseUrl "/nix/store/..." GotDrvDetails

-}
getDrvDetails : String -> String -> (Result Http.Error DrvDetails -> msg) -> Cmd msg
getDrvDetails apiBaseUrl drvPath toMsg =
    Http.get
        { url = apiBaseUrl ++ "/drvs/" ++ drvPath
        , expect = Http.expectJson toMsg Decoder.drvDetails
        }


{-| Get dependencies of a derivation.

    getDrvDependencies apiBaseUrl "/nix/store/..." GotDrvDependencies

-}
getDrvDependencies : String -> String -> (Result Http.Error (List DrvDependency) -> msg) -> Cmd msg
getDrvDependencies apiBaseUrl drvPath toMsg =
    Http.get
        { url = apiBaseUrl ++ "/drvs/" ++ drvPath ++ "/dependencies"
        , expect = Http.expectJson toMsg Decoder.drvDependencyList
        }


{-| Get all active builds (jobs with queued, buildable, or building drvs).

    getActiveBuilds apiBaseUrl GotActiveBuilds

-}
getActiveBuilds : String -> (Result Http.Error ActiveBuildsResponse -> msg) -> Cmd msg
getActiveBuilds apiBaseUrl toMsg =
    Http.get
        { url = apiBaseUrl ++ "/builds/active"
        , expect = Http.expectJson toMsg Decoder.activeBuildsResponse
        }


{-| Get all open pull requests with build statistics.

    getPullRequests apiBaseUrl GotPullRequests

-}
getPullRequests : String -> (Result Http.Error (List PullRequest) -> msg) -> Cmd msg
getPullRequests apiBaseUrl toMsg =
    Http.get
        { url = apiBaseUrl ++ "/prs"
        , expect = Http.expectJson toMsg Decoder.pullRequestList
        }


{-| Get a specific pull request with build statistics.

    getPullRequest apiBaseUrl "owner" "repo" 123 GotPullRequest

-}
getPullRequest : String -> String -> String -> Int -> (Result Http.Error PullRequest -> msg) -> Cmd msg
getPullRequest apiBaseUrl owner repo prNumber toMsg =
    Http.get
        { url = apiBaseUrl ++ "/prs/" ++ owner ++ "/" ++ repo ++ "/" ++ String.fromInt prNumber
        , expect = Http.expectJson toMsg Decoder.pullRequest
        }


{-| Get GitHub API metadata for a pull request (lines changed).

    getPRGitHubMetadata apiBaseUrl "owner" "repo" 123 GotPRMetadata

-}
getPRGitHubMetadata : String -> String -> String -> Int -> (Result Http.Error GitHubMetadata -> msg) -> Cmd msg
getPRGitHubMetadata apiBaseUrl owner repo prNumber toMsg =
    Http.get
        { url = apiBaseUrl ++ "/prs/" ++ owner ++ "/" ++ repo ++ "/" ++ String.fromInt prNumber ++ "/github-metadata"
        , expect = Http.expectJson toMsg Decoder.gitHubMetadata
        }

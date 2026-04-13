module Api.Decoder exposing
    ( activeBuildsResponse
    , buildState
    , buildingDrv
    , buildingDrvList
    , commitJob
    , commitJobList
    , drvDependency
    , drvDependencyList
    , drvDetails
    , gitHubMetadata
    , jobSetDetails
    , jobSetDrv
    , jobSetDrvList
    , maintainerDetail
    , maintainerDetailList
    , maintainerRequest
    , maintainerRequestList
    , pullRequest
    , pullRequestList
    , repository
    , repositoryList
    )

{-| JSON decoders for backend API responses.
-}

import Json.Decode as D exposing (Decoder)
import Models.BuildState as BS
import Models.Derivation exposing (DrvDependency, DrvDetails)
import Models.Job exposing (CommitJob, JobSetDetails, JobSetDrv)
import Models.Maintainer exposing (MaintainerDetail, MaintainerRequest, RequestStatus(..))
import Models.PullRequest exposing (GitHubMetadata, PullRequest)
import Models.Repository exposing (Repository)


{-| Decode a Repository from JSON.
-}
repository : Decoder Repository
repository =
    D.map3 Repository
        (D.field "owner" D.string)
        (D.field "repo_name" D.string)
        (D.field "installation_id" D.int)


{-| Decode a list of repositories.
-}
repositoryList : Decoder (List Repository)
repositoryList =
    D.list repository


{-| Decode a CommitJob from JSON.
-}
commitJob : Decoder CommitJob
commitJob =
    D.map5 CommitJob
        (D.field "jobset_id" D.int)
        (D.field "job_name" D.string)
        (D.field "sha" D.string)
        (D.field "owner" D.string)
        (D.field "repo_name" D.string)


{-| Decode a list of commit jobs.
-}
commitJobList : Decoder (List CommitJob)
commitJobList =
    D.list commitJob


{-| Decode JobSetDetails from JSON.
-}
jobSetDetails : Decoder JobSetDetails
jobSetDetails =
    D.succeed JobSetDetails
        |> andMap (D.field "jobset_id" D.int)
        |> andMap (D.field "sha" D.string)
        |> andMap (D.field "job_name" D.string)
        |> andMap (D.field "owner" D.string)
        |> andMap (D.field "repo_name" D.string)
        |> andMap (D.field "total_drvs" D.int)
        |> andMap (D.field "queued_drvs" D.int)
        |> andMap (D.field "buildable_drvs" D.int)
        |> andMap (D.field "building_drvs" D.int)
        |> andMap (D.field "completed_success_drvs" D.int)
        |> andMap (D.field "completed_failure_drvs" D.int)
        |> andMap (D.field "failed_retry_drvs" D.int)
        |> andMap (D.field "transitive_failure_drvs" D.int)
        |> andMap (D.field "blocked_drvs" D.int)
        |> andMap (D.field "interrupted_drvs" D.int)
        |> andMap (D.succeed 0)
        -- completedDrvs: computed field for backwards compat
        |> andMap (D.succeed 0)



-- failedDrvs: computed field for backwards compat


{-| Decode a JobSetDrv from JSON.
-}
jobSetDrv : Decoder JobSetDrv
jobSetDrv =
    D.succeed JobSetDrv
        |> andMap (D.field "drv_path" D.string)
        |> andMap (D.field "name" D.string)
        |> andMap (D.field "system" D.string)
        |> andMap (D.field "build_state" buildState)
        |> andMap (D.field "is_fod" D.bool)
        |> andMap (D.field "prefer_local_build" D.bool)


{-| Decode a list of jobset drvs.
-}
jobSetDrvList : Decoder (List JobSetDrv)
jobSetDrvList =
    D.list jobSetDrv


{-| Decode a JobDifference from JSON.
-}
jobDifference : Decoder Models.Job.JobDifference
jobDifference =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "New" ->
                        D.succeed Models.Job.New

                    "Changed" ->
                        D.succeed Models.Job.Changed

                    "Removed" ->
                        D.succeed Models.Job.Removed

                    _ ->
                        D.fail ("Unknown job difference: " ++ str)
            )


{-| Decode a BuildingDrv from JSON.
-}
buildingDrv : Decoder Models.Job.BuildingDrv
buildingDrv =
    D.succeed Models.Job.BuildingDrv
        |> andMap (D.field "drv_path" D.string)
        |> andMap (D.maybe (D.field "name" D.string))
        |> andMap (D.field "system" D.string)
        |> andMap (D.field "build_state" buildState)
        |> andMap (D.field "is_fod" D.bool)
        |> andMap (D.maybe (D.field "difference" jobDifference))


{-| Decode a list of building drvs.
-}
buildingDrvList : Decoder (List Models.Job.BuildingDrv)
buildingDrvList =
    D.list buildingDrv


{-| Decode DrvDetails from JSON.
-}
drvDetails : Decoder DrvDetails
drvDetails =
    D.succeed DrvDetails
        |> andMap (D.field "drv_path" D.string)
        |> andMap (D.field "name" D.string)
        |> andMap (D.field "system" D.string)
        |> andMap (D.field "build_state" buildState)
        |> andMap (D.field "is_fod" D.bool)
        |> andMap (D.field "prefer_local_build" D.bool)


{-| Decode a DrvDependency from JSON.
-}
drvDependency : Decoder DrvDependency
drvDependency =
    D.map3 DrvDependency
        (D.field "drv_path" D.string)
        (D.field "name" D.string)
        (D.field "build_state" buildState)


{-| Decode a list of drv dependencies.
-}
drvDependencyList : Decoder (List DrvDependency)
drvDependencyList =
    D.field "dependencies" (D.list drvDependency)


{-| Decode a DrvBuildState from JSON.

The backend serializes this as a tagged union, e.g.:

  - "Queued"
  - "Buildable"
  - {"Completed": "Success"}
  - {"Interrupted": "Timeout"}

-}
buildState : Decoder BS.DrvBuildState
buildState =
    D.oneOf
        [ D.string
            |> D.andThen
                (\str ->
                    case str of
                        "Queued" ->
                            D.succeed BS.Queued

                        "Buildable" ->
                            D.succeed BS.Buildable

                        "FailedRetry" ->
                            D.succeed BS.FailedRetry

                        "Building" ->
                            D.succeed BS.Building

                        "TransitiveFailure" ->
                            D.succeed BS.TransitiveFailure

                        "Blocked" ->
                            D.succeed BS.Blocked

                        _ ->
                            D.fail ("Unknown build state: " ++ str)
                )
        , D.field "Completed" buildResult
            |> D.map BS.Completed
        , D.field "Interrupted" interruptionKind
            |> D.map BS.Interrupted
        ]


{-| Decode a DrvBuildResult.
-}
buildResult : Decoder BS.DrvBuildResult
buildResult =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "Success" ->
                        D.succeed BS.Success

                    "Failure" ->
                        D.succeed BS.Failure

                    _ ->
                        D.fail ("Unknown build result: " ++ str)
            )


{-| Decode a DrvBuildInterruptionKind.
-}
interruptionKind : Decoder BS.DrvBuildInterruptionKind
interruptionKind =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "OutOfMemory" ->
                        D.succeed BS.OutOfMemory

                    "Timeout" ->
                        D.succeed BS.Timeout

                    "Cancelled" ->
                        D.succeed BS.Cancelled

                    "ProcessDeath" ->
                        D.succeed BS.ProcessDeath

                    _ ->
                        D.fail ("Unknown interruption kind: " ++ str)
            )


{-| Decode active builds response from JSON.
-}
activeBuildsResponse : Decoder { jobs : List Models.Job.JobSetDetails, buildingDrvs : List Models.Job.BuildingDrv }
activeBuildsResponse =
    D.map2 (\jobs buildingDrvs -> { jobs = jobs, buildingDrvs = buildingDrvs })
        (D.field "jobs" (D.list jobSetDetails))
        (D.field "building_drvs" buildingDrvList)


{-| Decode a PullRequest from JSON.
-}
pullRequest : Decoder PullRequest
pullRequest =
    D.succeed PullRequest
        |> andMap (D.field "pr_number" D.int)
        |> andMap (D.field "owner" D.string)
        |> andMap (D.field "repo_name" D.string)
        |> andMap (D.field "head_sha" D.string)
        |> andMap (D.field "base_sha" D.string)
        |> andMap (D.field "title" D.string)
        |> andMap (D.field "author" D.string)
        |> andMap (D.field "state" D.string)
        |> andMap (D.field "created_at" D.string)
        |> andMap (D.field "updated_at" D.string)
        |> andMap (D.maybe (D.field "jobset_id" D.int))
        |> andMap (D.field "total_drvs" D.int)
        |> andMap (D.field "completed_success_drvs" D.int)
        |> andMap (D.field "completed_failure_drvs" D.int)
        |> andMap (D.field "failed_retry_drvs" D.int)
        |> andMap (D.field "changed_drvs" D.int)
        |> andMap (D.field "new_drvs" D.int)


{-| Decode a list of pull requests.
-}
pullRequestList : Decoder (List PullRequest)
pullRequestList =
    D.list pullRequest


{-| Decode GitHub metadata for a pull request.
-}
gitHubMetadata : Decoder GitHubMetadata
gitHubMetadata =
    D.map3 GitHubMetadata
        (D.field "additions" D.int)
        (D.field "deletions" D.int)
        (D.field "changed_files" D.int)


{-| Decode a MaintainerDetail from JSON.
-}
maintainerDetail : Decoder MaintainerDetail
maintainerDetail =
    D.succeed MaintainerDetail
        |> andMap (D.field "attr_path" D.string)
        |> andMap (D.field "github_user_id" D.int)
        |> andMap (D.field "github_username" D.string)
        |> andMap (D.maybe (D.field "github_avatar_url" D.string))
        |> andMap (D.field "added_at" D.string)
        |> andMap (D.maybe (D.field "added_by_user_id" D.int))
        |> andMap (D.maybe (D.field "added_by_username" D.string))


{-| Decode a list of maintainer details.
-}
maintainerDetailList : Decoder (List MaintainerDetail)
maintainerDetailList =
    D.list maintainerDetail


{-| Decode a RequestStatus from JSON.
-}
requestStatus : Decoder RequestStatus
requestStatus =
    D.string
        |> D.andThen
            (\str ->
                case str of
                    "approved" ->
                        D.succeed Approved

                    "rejected" ->
                        D.succeed Rejected

                    "pending" ->
                        D.succeed Pending

                    _ ->
                        D.succeed Pending
            )


{-| Decode a MaintainerRequest from JSON.
-}
maintainerRequest : Decoder MaintainerRequest
maintainerRequest =
    D.succeed MaintainerRequest
        |> andMap (D.field "id" D.int)
        |> andMap (D.field "attr_path" D.string)
        |> andMap (D.field "github_user_id" D.int)
        |> andMap (D.field "github_username" D.string)
        |> andMap (D.maybe (D.field "github_avatar_url" D.string))
        |> andMap (D.field "requested_at" D.string)
        |> andMap (D.field "status" requestStatus)
        |> andMap (D.maybe (D.field "reviewed_by_user_id" D.int))
        |> andMap (D.maybe (D.field "reviewed_by_username" D.string))
        |> andMap (D.maybe (D.field "reviewed_at" D.string))


{-| Decode a list of maintainer requests.
-}
maintainerRequestList : Decoder (List MaintainerRequest)
maintainerRequestList =
    D.list maintainerRequest



-- Helper for pipeline-style decoding


andMap : Decoder a -> Decoder (a -> b) -> Decoder b
andMap =
    D.map2 (|>)

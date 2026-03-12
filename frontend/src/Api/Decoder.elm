module Api.Decoder exposing
    ( repository
    , repositoryList
    , commitJob
    , commitJobList
    , jobSetDetails
    , jobSetDrv
    , jobSetDrvList
    , drvDetails
    , drvDependency
    , drvDependencyList
    , buildState
    )

{-| JSON decoders for backend API responses.
-}

import Json.Decode as D exposing (Decoder)
import Models.BuildState as BS
import Models.Derivation exposing (DrvDependency, DrvDetails)
import Models.Job exposing (CommitJob, JobSetDetails, JobSetDrv)
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
        |> andMap (D.field "job" D.string)
        |> andMap (D.field "owner" D.string)
        |> andMap (D.field "repo_name" D.string)
        |> andMap (D.field "total_drvs" D.int)
        |> andMap (D.field "queued_drvs" D.int)
        |> andMap (D.field "buildable_drvs" D.int)
        |> andMap (D.field "building_drvs" D.int)
        |> andMap (D.field "completed_drvs" D.int)
        |> andMap (D.field "failed_drvs" D.int)
        |> andMap (D.field "transitive_failure_drvs" D.int)


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



-- Helper for pipeline-style decoding


andMap : Decoder a -> Decoder (a -> b) -> Decoder b
andMap =
    D.map2 (|>)

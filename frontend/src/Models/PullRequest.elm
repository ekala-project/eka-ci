module Models.PullRequest exposing
    ( GitHubMetadata
    , PullRequest
    , PullRequestInfo
    )

{-| Pull Request models matching the backend API response.

Represents pull requests and their build statistics.

-}


{-| Basic PR information.
-}
type alias PullRequestInfo =
    { prNumber : Int
    , owner : String
    , repoName : String
    , headSha : String
    , baseSha : String
    , title : String
    , author : String
    , state : String
    , createdAt : String
    , updatedAt : String
    }


{-| A pull request with associated build statistics.
-}
type alias PullRequest =
    { prNumber : Int
    , owner : String
    , repoName : String
    , headSha : String
    , baseSha : String
    , title : String
    , author : String
    , state : String
    , createdAt : String
    , updatedAt : String
    , jobsetId : Maybe Int
    , totalDrvs : Int
    , completedSuccessDrvs : Int
    , completedFailureDrvs : Int
    , failedRetryDrvs : Int
    , changedDrvs : Int
    , newDrvs : Int
    }


{-| GitHub API metadata for a PR (lines changed).
-}
type alias GitHubMetadata =
    { additions : Int
    , deletions : Int
    , changedFiles : Int
    }

module Models.Maintainer exposing
    ( MaintainerDetail
    , MaintainerRequest
    , RequestStatus(..)
    )

{-| Types for attr path maintainers and maintainer requests.
-}


{-| Detailed information about a maintainer including who added them.
-}
type alias MaintainerDetail =
    { attrPath : String
    , githubUserId : Int
    , githubUsername : String
    , githubAvatarUrl : Maybe String
    , addedAt : String
    , addedByUserId : Maybe Int
    , addedByUsername : Maybe String
    }


{-| A request to become a maintainer of an attr path.
-}
type alias MaintainerRequest =
    { id : Int
    , attrPath : String
    , githubUserId : Int
    , githubUsername : String
    , githubAvatarUrl : Maybe String
    , requestedAt : String
    , status : RequestStatus
    , reviewedByUserId : Maybe Int
    , reviewedByUsername : Maybe String
    , reviewedAt : Maybe String
    }


{-| Status of a maintainer request.
-}
type RequestStatus
    = Pending
    | Approved
    | Rejected


{-| Convert a string to a RequestStatus.
-}
requestStatusFromString : String -> RequestStatus
requestStatusFromString str =
    case str of
        "approved" ->
            Approved

        "rejected" ->
            Rejected

        _ ->
            Pending


{-| Convert a RequestStatus to a string.
-}
requestStatusToString : RequestStatus -> String
requestStatusToString status =
    case status of
        Pending ->
            "pending"

        Approved ->
            "approved"

        Rejected ->
            "rejected"

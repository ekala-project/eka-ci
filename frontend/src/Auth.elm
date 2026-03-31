module Auth exposing
    ( AuthState
    , User
    , decodeLoginResponse
    , decodeUser
    , getUserInfo
    , init
    , isAdmin
    , isAuthenticated
    , login
    , logout
    , updateToken
    , updateUser
    )

{-| Authentication module for EkaCI.

Handles GitHub OAuth authentication, JWT token management,
and user state.

-}

import Json.Decode as D exposing (Decoder)


{-| Authentication state.
-}
type alias AuthState =
    { token : Maybe String
    , user : Maybe User
    , loading : Bool
    }


{-| User information.
-}
type alias User =
    { githubId : Int
    , username : String
    , avatarUrl : String
    , isAdmin : Bool
    }


{-| Initialize empty auth state.
-}
init : AuthState
init =
    { token = Nothing
    , user = Nothing
    , loading = False
    }


{-| Update the token in auth state.
-}
updateToken : Maybe String -> AuthState -> AuthState
updateToken token state =
    { state | token = token }


{-| Update the user in auth state.
-}
updateUser : Maybe User -> AuthState -> AuthState
updateUser user state =
    { state | user = user, loading = False }


{-| Check if user is authenticated.
-}
isAuthenticated : AuthState -> Bool
isAuthenticated state =
    state.token /= Nothing && state.user /= Nothing


{-| Check if user is an admin.
-}
isAdmin : AuthState -> Bool
isAdmin state =
    case state.user of
        Just user ->
            user.isAdmin

        Nothing ->
            False


{-| Start login process.

This returns the GitHub OAuth URL to redirect to.

-}
login : String
login =
    "/github/auth/login"


{-| Logout by clearing auth state.
-}
logout : AuthState -> AuthState
logout state =
    { token = Nothing
    , user = Nothing
    , loading = False
    }


{-| Get user info API endpoint.
-}
getUserInfo : String
getUserInfo =
    "/github/auth/me"


{-| Decode user from JSON.
-}
decodeUser : Decoder User
decodeUser =
    D.map4 User
        (D.field "github_id" D.int)
        (D.field "username" D.string)
        (D.field "avatar_url" D.string)
        (D.field "is_admin" D.bool)


{-| Decode login response from OAuth callback.

The backend returns: { "token": "jwt...", "user": {...} }

-}
decodeLoginResponse : Decoder { token : String, user : User }
decodeLoginResponse =
    D.map2 (\token user -> { token = token, user = user })
        (D.field "token" D.string)
        (D.field "user" decodeUser)

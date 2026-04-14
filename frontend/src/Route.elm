module Route exposing
    ( Route(..)
    , fromUrl
    , pushUrl
    , toHref
    )

{-| Routing for the application.

Routes map to different pages in the app.

-}

import Browser.Navigation as Nav
import Url exposing (Url)
import Url.Builder as UB
import Url.Parser as P exposing ((</>), Parser)


{-| The possible routes in the application.
-}
type Route
    = Home
    | Builds -- Active builds page
    | Reviews -- PR review portal
    | PullRequest String String Int -- owner, repo, pr_number
    | Repository String String -- owner, repo
    | Commit String -- sha
    | Job Int -- jobset_id
    | Drv String -- drv_path
    | Admin
    | Profile -- User profile
    | AttrPathSearch -- Attr path search page
    | AuthCallback -- GitHub OAuth callback
    | NotFound


{-| Parse a URL into a Route.
-}
fromUrl : Url -> Route
fromUrl url =
    P.parse routeParser url
        |> Maybe.withDefault NotFound


{-| URL parser for routes.
-}
routeParser : Parser (Route -> a) a
routeParser =
    P.oneOf
        [ P.map Home P.top
        , P.map Builds (P.s "builds")
        , P.map Reviews (P.s "reviews")
        , P.map PullRequest (P.s "prs" </> P.string </> P.string </> P.int)
        , P.map Repository (P.s "repos" </> P.string </> P.string)
        , P.map Commit (P.s "commits" </> P.string)
        , P.map Job (P.s "jobs" </> P.int)
        , P.map Drv (P.s "drvs" </> P.string)
        , P.map Admin (P.s "admin")
        , P.map Profile (P.s "profile")
        , P.map AttrPathSearch (P.s "attr-paths")
        , P.map AuthCallback (P.s "github" </> P.s "auth" </> P.s "callback")
        ]


{-| Convert a Route to an href attribute value.
-}
toHref : Route -> String
toHref route =
    case route of
        Home ->
            "/"

        Builds ->
            "/builds"

        Reviews ->
            "/reviews"

        PullRequest owner repo prNumber ->
            UB.absolute [ "prs", owner, repo, String.fromInt prNumber ] []

        Repository owner repo ->
            UB.absolute [ "repos", owner, repo ] []

        Commit sha ->
            UB.absolute [ "commits", sha ] []

        Job jobsetId ->
            UB.absolute [ "jobs", String.fromInt jobsetId ] []

        Drv drvPath ->
            UB.absolute [ "drvs", drvPath ] []

        Admin ->
            "/admin"

        Profile ->
            "/profile"

        AttrPathSearch ->
            "/attr-paths"

        AuthCallback ->
            "/github/auth/callback"

        NotFound ->
            "/"


{-| Navigate to a route using pushUrl.
-}
pushUrl : Nav.Key -> Route -> Cmd msg
pushUrl key route =
    Nav.pushUrl key (toHref route)

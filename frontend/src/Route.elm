module Route exposing
    ( Route(..)
    , fromUrl
    , toHref
    , pushUrl
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
    | Repository String String -- owner, repo
    | Commit String -- sha
    | Job Int -- jobset_id
    | Drv String -- drv_path
    | Admin
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
        , P.map Repository (P.s "repos" </> P.string </> P.string)
        , P.map Commit (P.s "commits" </> P.string)
        , P.map Job (P.s "jobs" </> P.int)
        , P.map Drv (P.s "drvs" </> P.string)
        , P.map Admin (P.s "admin")
        ]


{-| Convert a Route to an href attribute value.
-}
toHref : Route -> String
toHref route =
    case route of
        Home ->
            "/"

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

        NotFound ->
            "/"


{-| Navigate to a route using pushUrl.
-}
pushUrl : Nav.Key -> Route -> Cmd msg
pushUrl key route =
    Nav.pushUrl key (toHref route)

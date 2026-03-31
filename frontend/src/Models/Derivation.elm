module Models.Derivation exposing
    ( DrvDependency
    , DrvDetails
    )

{-| Derivation models matching the backend API responses.
-}

import Models.BuildState exposing (DrvBuildState)


{-| Detailed information about a derivation (from GET /v1/drvs/{drv}).
-}
type alias DrvDetails =
    { drvPath : String
    , name : String
    , system : String
    , buildState : DrvBuildState
    , isFod : Bool
    , preferLocalBuild : Bool
    }


{-| A dependency of a derivation (from GET /v1/drvs/{drv}/dependencies).
-}
type alias DrvDependency =
    { drvPath : String
    , name : String
    , buildState : DrvBuildState
    }

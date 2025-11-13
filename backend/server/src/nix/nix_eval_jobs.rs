use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum NixEvalItem {
    Error(NixEvalError),
    Drv(NixEvalDrv),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NixEvalDrv {
    /// String of full attr path
    /// E.g. "python.pkgs.setuptools"
    pub attr: String,

    /// List of attrs to access drv.
    /// "python.pkgs.setuptools" -> [ "python" "pkgs" "setuptools" ]
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,

    /// Store path to drv. E.g. "/nix/store/<hash>-<name>.drv"
    #[serde(rename = "drvPath")]
    pub drv_path: String,

    /// Name of drv. Usually includes "${pname}-${version}", but doesn't need to
    pub name: String,

    /// A mapping of the multiple outputs and their respective nix store paths
    pub outputs: HashMap<String, String>,

    /// Build platform system
    pub system: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NixEvalError {
    pub attr: String,
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialization() {
        // Generated with:
        // `nix-eval-jobs --expr 'with import ./. {}; { inherit grpc; }'`
        // while in nixpkgs directory
        let eval_drv = r#"{"attr":"grpc","attrPath":["grpc"],"drvPath":"/nix/store/qkgrb8v2ikxphb8raj8s0wd5rd7aip32-grpc-1.70.0.drv","name":"grpc-1.70.0","outputs":{"out":"/nix/store/rn3nlskr54yvw9gqq8im2g6c5bjyqqb5-grpc-1.70.0"},"system":"x86_64-linux"}"#;

        serde_json::from_str::<NixEvalDrv>(eval_drv).expect("Failed to deserialize output");
    }

    #[test]
    fn test_error() {
        let err = r##"{"attr":"adoptopenjdk-openj9-bin-15","attrPath":["adoptopenjdk-openj9-bin-15"],"error":"error:\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:7:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |       ^\n          218|     ) aliases;\n\n       … while calling anonymous lambda\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:10:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |          ^\n          218|     ) aliases;\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:17:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                 ^\n          218|     ) aliases;\n\n       … while calling 'removeDistribute'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:22:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                      ^\n           35|\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:29:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                             ^\n           35|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:32:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                                ^\n           35|\n\n       … while calling 'isDerivation'\n         at /home/jon/projects/nixpkgs/lib/attrsets.nix:1251:18:\n         1250|   */\n         1251|   isDerivation = value: value.type or null == \"derivation\";\n             |                  ^\n         1252|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:35:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                   ^\n          218|     ) aliases;\n\n       … while calling 'removeRecurseForDerivations'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:26:5:\n           25|   removeRecurseForDerivations =\n           26|     alias:\n             |     ^\n           27|     if alias.recurseForDerivations or false then\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:27:5:\n           26|     alias:\n           27|     if alias.recurseForDerivations or false then\n             |     ^\n           28|       lib.removeAttrs alias [ \"recurseForDerivations\" ]\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:64:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                                                ^\n          218|     ) aliases;\n\n       … while calling 'checkInPkgs'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:211:8:\n          210|   checkInPkgs =\n          211|     n: alias:\n             |        ^\n          212|     if builtins.hasAttr n super then throw \"Alias ${n} is still in all-packages.nix\" else alias;\n\n       … while calling the 'throw' builtin\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:257:32:\n          256|   adoptopenjdk-openj9-bin-11 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. Consider using `semeru-bin-11`.\"; # Added 2024-05-09\n          257|   adoptopenjdk-openj9-bin-15 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n             |                                ^\n          258|   adoptopenjdk-openj9-bin-16 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 16 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n\n       error: adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`."}"##;
        let _item = serde_json::from_str::<NixEvalItem>(err).expect("Failed to deserialize output");
    }
}

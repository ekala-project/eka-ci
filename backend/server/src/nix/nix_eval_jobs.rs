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

    /// Direct references/dependencies of this drv
    #[serde(rename = "inputDrvs")]
    pub input_drvs: Option<HashMap<String, Vec<String>>>,

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
        let eval_drv = r#"{"attr":"cmake","attrPath":["cmake"],"drvPath":"/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv","inputDrvs":{"/nix/store/08s4j5nvddsbrjpachqwzai83xngxnc0-pkg-config-wrapper-0.29.2.drv":["out"],"/nix/store/0cgbdlz63qiqf5f8i1sljak1dfbzyrl5-openssl-3.0.14.drv":["dev"],"/nix/store/265x0i426vnqjma9khcfpi86m6hx4smr-bash-5.2p32.drv":["out"],"/nix/store/27zlixdsk0kx585j4dcjm53636mx7cis-libuv-1.48.0.drv":["dev"],"/nix/store/2vyizsckka60lhh0kylhbpdd1flb998v-cmake-3.29.6.tar.gz.drv":["out"],"/nix/store/4hzjv6r5v7h6hzad718jgc0hrm1gz8r1-gcc-wrapper-13.3.0.drv":["out"],"/nix/store/860zddz386bk0441flrg940ipbp0jp1z-xz-5.6.2.drv":["dev"],"/nix/store/9jvlq6qg9j1222w3zm3wgfv5qyqfqmxz-bzip2-1.0.8.drv":["dev"],"/nix/store/ax4q30iyf9wi95hswil021lg0cdqq6rl-libarchive-3.7.4.drv":["dev"],"/nix/store/bxq3kjf71wn92yisdbq18fzpvcl5pn31-expat-2.6.2.drv":["dev"],"/nix/store/kh6mps96srqgdvn03vq4gmqzl51s9w8h-glibc-2.39-52.drv":["bin","dev","out"],"/nix/store/lzc503qcc7f6ibq8sdbcri73wb62dj4r-zlib-1.3.1.drv":["dev"],"/nix/store/mzw7jzs6ix17ajh3z4kqzvh8l7abj4yr-rhash-1.4.4.drv":["out"],"/nix/store/v288gxsg679gyi9zpg0mhrv26vfmw4kr-stdenv-linux.drv":["out"],"/nix/store/vnq47hr4nwry8kgvfgmx0229id3q49dr-binutils-2.42.drv":["out"],"/nix/store/y99v9h2mcqbw91g7p3lnk292k0np0djr-curl-8.9.0.drv":["dev"]},"name":"cmake-3.29.6","outputs":{"debug":"/nix/store/xrh9g28kmsyjlw6qf46ngkvhac1llgvz-cmake-3.29.6-debug","out":"/nix/store/rz7j0kdkq8j522vpw6n8wjq2qv3if24g-cmake-3.29.6"},"system":"x86_64-linux"}"#;

        serde_json::from_str::<NixEvalDrv>(eval_drv).expect("Failed to deserialize output");
    }

    #[test]
    fn test_error() {
        let err = r##"{"attr":"adoptopenjdk-openj9-bin-15","attrPath":["adoptopenjdk-openj9-bin-15"],"error":"error:\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:7:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |       ^\n          218|     ) aliases;\n\n       … while calling anonymous lambda\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:10:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |          ^\n          218|     ) aliases;\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:17:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                 ^\n          218|     ) aliases;\n\n       … while calling 'removeDistribute'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:22:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                      ^\n           35|\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:29:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                             ^\n           35|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:34:32:\n           33|   # sets from building on Hydra.\n           34|   removeDistribute = alias: if lib.isDerivation alias then lib.dontDistribute alias else alias;\n             |                                ^\n           35|\n\n       … while calling 'isDerivation'\n         at /home/jon/projects/nixpkgs/lib/attrsets.nix:1251:18:\n         1250|   */\n         1251|   isDerivation = value: value.type or null == \"derivation\";\n             |                  ^\n         1252|\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:35:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                   ^\n          218|     ) aliases;\n\n       … while calling 'removeRecurseForDerivations'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:26:5:\n           25|   removeRecurseForDerivations =\n           26|     alias:\n             |     ^\n           27|     if alias.recurseForDerivations or false then\n\n       … while evaluating a branch condition\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:27:5:\n           26|     alias:\n           27|     if alias.recurseForDerivations or false then\n             |     ^\n           28|       lib.removeAttrs alias [ \"recurseForDerivations\" ]\n\n       … from call site\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:217:64:\n          216|     lib.mapAttrs (\n          217|       n: alias: removeDistribute (removeRecurseForDerivations (checkInPkgs n alias))\n             |                                                                ^\n          218|     ) aliases;\n\n       … while calling 'checkInPkgs'\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:211:8:\n          210|   checkInPkgs =\n          211|     n: alias:\n             |        ^\n          212|     if builtins.hasAttr n super then throw \"Alias ${n} is still in all-packages.nix\" else alias;\n\n       … while calling the 'throw' builtin\n         at /home/jon/projects/nixpkgs/pkgs/top-level/aliases.nix:257:32:\n          256|   adoptopenjdk-openj9-bin-11 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. Consider using `semeru-bin-11`.\"; # Added 2024-05-09\n          257|   adoptopenjdk-openj9-bin-15 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n             |                                ^\n          258|   adoptopenjdk-openj9-bin-16 = throw \"adoptopenjdk has been removed as the upstream project is deprecated. JDK 16 is also EOL. Consider using `semeru-bin-17`.\"; # Added 2024-05-09\n\n       error: adoptopenjdk has been removed as the upstream project is deprecated. JDK 15 is also EOL. Consider using `semeru-bin-17`."}"##;
        let _item = serde_json::from_str::<NixEvalItem>(err).expect("Failed to deserialize output");
    }
}

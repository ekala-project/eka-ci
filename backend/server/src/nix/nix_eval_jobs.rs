use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
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

    /// A mapping of drv dependencies to their realized outputs which will
    /// be introduced to a build.
    /// For EkaCI, we are less concerned about which outputs are used, and
    /// rather more sensitive to whether the dependencies build
    #[serde(rename = "inputDrvs")]
    pub input_drvs: HashMap<String, Vec<String>>,

    /// Name of drv. Usually includes "${pname}-${version}", but doesn't need to
    pub name: String,

    /// A mapping of the multiple outputs and their respective nix store paths
    pub outputs: HashMap<String, String>,

    /// Build platform system
    pub system: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialization() {
        // Generated with:
        // `nix-eval-jobs --expr 'with import ./. {}; { inherit grpc; }'`
        // while in nixpkgs directory
        let eval_drv =
r#"{"attr":"grpc","attrPath":["grpc"],"drvPath":"/nix/store/qkgrb8v2ikxphb8raj8s0wd5rd7aip32-grpc-1.70.0.drv","inputDrvs":{"/nix/store/0g39h9hxzcayx0vhnk5rxw4w6w0a1ysg-protobuf-29.3.drv":["out"],"/nix/store/27js0n1n41i6llkwyx0cxjjv69s9wsnl-libnsl-2.0.1.drv":["dev"],"/nix/store/3xazsgw9bqdyija05ln1h4bdnglwszfv-grpc-link-libatomic.patch.drv":["out"],"/nix/store/cfp8jh04f3jfdcjskw2p64ri3w6njndm-bash-5.2p37.drv":["out"],"/nix/store/f3a1xni8b1m14ypk542mgwm09xc38ixv-re2-2024-07-02.drv":["dev"],"/nix/store/ff2x0aw6pa3wrrb7c7qwillijnwci187-pkg-config-wrapper-0.29.2.drv":["out"],"/nix/store/m6fq0k2l3kgbxabywyl8sih5mfnf6fa2-zlib-1.3.1.drv":["dev"],"/nix/store/nmkwhi73566k4w1p1ryrw4s8sa1jln36-cmake-3.31.5.drv":["out"],"/nix/store/x0rh46f5izp9ylg4sryzdm74zj70az9f-abseil-cpp-20240722.1.drv":["out"],"/nix/store/x6k83rqd620y3zsb5cdj4p85d1ad6pql-source.drv":["out"],"/nix/store/xx2ps0lnvwv755a0pbyzlg0p68zhqvnl-stdenv-linux.drv":["out"],"/nix/store/z1hzmg700hskbh8al2zd3vxiirmklgk6-openssl-3.4.1.drv":["dev"],"/nix/store/zbs03n3c4hwndqfkfbcshvwkcg3yin73-c-ares-1.34.4.drv":["dev"]},"name":"grpc-1.70.0","outputs":{"out":"/nix/store/rn3nlskr54yvw9gqq8im2g6c5bjyqqb5-grpc-1.70.0"},"system":"x86_64-linux"}"#;

        serde_json::from_str::<NixEvalDrv>(eval_drv).expect("Failed to deserialize output");
    }
}


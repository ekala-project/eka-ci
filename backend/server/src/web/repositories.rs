use axum::Json;
use serde::Serialize;
use url::Url;

#[derive(Debug, Serialize)]
pub(super) struct Repository {
    owner: String,
    name: String,
    description: String,
    url: Url,
    avatar_url: Url,
}

pub(super) async fn list() -> Json<Vec<Repository>> {
    let repos = vec![
        Repository {
            owner: "ekala-project".to_owned(),
            name: "eka-ci".to_owned(),
            description: "CI/CD tool and web frontend for nix package sets".to_owned(),
            url: "https://github.com/ekala-project/eka-ci"
                .try_into()
                .expect("valid"),
            avatar_url: "https://avatars.githubusercontent.com/u/172489582?v=4"
                .try_into()
                .expect("valid"),
        },
        Repository {
            owner: "rust-lang".to_owned(),
            name: "rust".to_owned(),
            description: "Empowering everyone to build reliable and efficient software.".to_owned(),
            url: "https://github.com/rust-lang/rust"
                .try_into()
                .expect("valid"),
            avatar_url: "https://avatars.githubusercontent.com/u/5430905?v=4"
                .try_into()
                .expect("valid"),
        },
        Repository {
            owner: "elm".to_owned(),
            name: "compiler".to_owned(),
            description: "Compiler for Elm, a functional language for reliable webapps.".to_owned(),
            url: "https://github.com/elm/compiler".try_into().expect("valid"),
            avatar_url: "https://avatars.githubusercontent.com/u/20698192?v=4"
                .try_into()
                .expect("valid"),
        },
        Repository {
            owner: "NixOS".to_owned(),
            name: "nixpkgs".to_owned(),
            description: "Nix Packages collection & NixOS".to_owned(),
            url: "https://github.com/NixOS/nixpkgs"
                .try_into()
                .expect("valid"),
            avatar_url: "https://avatars.githubusercontent.com/u/487568?v=4"
                .try_into()
                .expect("valid"),
        },
    ];
    Json(repos)
}

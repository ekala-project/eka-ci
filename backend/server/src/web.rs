use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use axum::{routing::get, Router};
use http::StatusCode;
use tokio::net::TcpListener;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_status::SetStatus,
};

pub struct WebService {
    listener: TcpListener,
}

impl WebService {
    pub async fn bind_to_addr_and_port(addr: String, port: u16) -> Result<Self> {
        let web_listen_address = addr
            .parse::<Ipv4Addr>()
            .context("failed to determine listen address")?;

        let listener = tokio::net::TcpListener::bind((web_listen_address, port))
            .await
            .context(format!(
                "failed to bind to tcp socket at {web_listen_address}:{port}"
            ))?;

        Ok(Self { listener })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self, spa_bundle_path: Option<PathBuf>) {
        let app = Router::new().nest("/api", api_routes());

        let app = if let Some(spa_bundle_path) = spa_bundle_path {
            // If nothing else matched, always return the SPA. The client application has its own
            // router, which it will use to handle the requested the path.
            app.fallback_service(spa_service(&spa_bundle_path))
        } else {
            // Make sure to include some information on why there is no UI showing.
            app.fallback(|| async {
                (
                    StatusCode::NOT_FOUND,
                    "This instance of Eka CI has been started with the web interface disabled.",
                )
            })
        };

        axum::serve(self.listener, app)
            .await
            .expect("axum::serve never returns");
    }
}

fn api_routes() -> Router {
    // Placeholder to verify that nesting works as expected.
    Router::new().route("/", get(|| async { "API" }))
}

fn spa_service(bundle: &Path) -> ServeDir<SetStatus<ServeFile>> {
    // The recommended way to serve a SPA:
    // https://github.com/tokio-rs/axum/blob/main/axum-extra/CHANGELOG.md#060-24-february-2022
    ServeDir::new(bundle).not_found_service(ServeFile::new(bundle.join("index.html")))
}

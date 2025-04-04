use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};
#[cfg(feature = "bundle-proxy")]
use std::num::NonZeroU16;

use anyhow::{Context, Result};
use axum::{routing::get, Router};
use http::StatusCode;
use tokio::net::TcpListener;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_status::SetStatus,
};

use crate::cli::Args;

pub enum SpaBundle {
    Disabled,
    Path(PathBuf),
    #[cfg(feature = "bundle-proxy")]
    Proxy(NonZeroU16),
}

impl SpaBundle {
    pub fn from_args(args: &Args) -> Self {
        let spa_bundle = args.bundle.clone().map(SpaBundle::Path);
        #[cfg(feature = "bundle-proxy")]
        let spa_bundle = spa_bundle.or(args.bundle_port.map(SpaBundle::Proxy));
        spa_bundle.unwrap_or(SpaBundle::Disabled)
    }
}

impl std::fmt::Display for SpaBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpaBundle::Disabled => write!(f, "frontend disabled"),
            SpaBundle::Path(path) => write!(
                f,
                "frontend from {}",
                path.canonicalize().unwrap_or(path.to_owned()).display()
            ),
            #[cfg(feature = "bundle-proxy")]
            SpaBundle::Proxy(port) => write!(f, "frontend from http://127.0.0.1:{port}"),
        }
    }
}

pub struct WebService {
    listener: TcpListener,
    spa_bundle: SpaBundle,
}

impl WebService {
    pub async fn bind_from_args(args: &Args) -> Result<Self> {
        let listener = bind_to_addr_and_port(&args.addr, args.port).await?;
        let spa_bundle = SpaBundle::from_args(args);
        Ok(Self {
            listener,
            spa_bundle,
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub fn spa_bundle(&self) -> &SpaBundle {
        &self.spa_bundle
    }

    pub async fn run(self) {
        let app = Router::new().nest("/api", api_routes());

        let app = match self.spa_bundle {
            SpaBundle::Path(spa_bundle_path) => {
                // If nothing else matched, always return the SPA. The client application has its own
                // router, which it will use to handle the requested the path.
                app.fallback_service(spa_service(&spa_bundle_path))
            }
            #[cfg(feature = "bundle-proxy")]
            SpaBundle::Proxy(spa_proxy_port) => {
                use axum_reverse_proxy::ReverseProxy;
                let proxy: Router =
                    ReverseProxy::new("", &format!("http://127.0.0.1:{spa_proxy_port}")).into();

                app.fallback_service(proxy)
            }
            SpaBundle::Disabled => {
                // Make sure to include some information on why there is no UI showing.
                app.fallback(|| async {
                    (
                        StatusCode::NOT_FOUND,
                        "This instance of Eka CI has been started with the web interface disabled.",
                    )
                })
            }
        };

        axum::serve(self.listener, app)
            .await
            .expect("axum::serve never returns");
    }
}

async fn bind_to_addr_and_port(addr: &str, port: u16) -> Result<TcpListener> {
    let web_listen_address = addr
        .parse::<Ipv4Addr>()
        .context("failed to determine listen address")?;

    let listener = tokio::net::TcpListener::bind((web_listen_address, port))
        .await
        .context(format!(
            "failed to bind to tcp socket at {web_listen_address}:{port}"
        ))?;

    Ok(listener)
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

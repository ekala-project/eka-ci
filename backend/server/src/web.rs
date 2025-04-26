use std::net::{SocketAddr, SocketAddrV4};

use anyhow::{Context, Result};
use axum::{routing::get, Router};
use tokio::net::TcpListener;

pub struct WebService {
    listener: TcpListener,
}

impl WebService {
    pub async fn bind_to_address(socket: &SocketAddrV4) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        Ok(Self { listener })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self) {
        let app = Router::new().nest("/v1", api_routes());

        axum::serve(self.listener, app)
            .await
            .expect("axum::serve never returns");
    }
}

fn api_routes() -> Router {
    Router::new().route("/logs/{drv}", get(get_derivation_log))
}

async fn get_derivation_log(axum::extract::Path(drv): axum::extract::Path<String>) -> String {
    format!("Dummy log data for {drv}")
}

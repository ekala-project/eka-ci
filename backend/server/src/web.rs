use std::path::{Path, PathBuf};

use axum::{routing::get, Router};
use http::StatusCode;
use tokio::net::TcpListener;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_status::SetStatus,
};

pub async fn serve_web(listener: TcpListener, bundle: Option<PathBuf>) {
    let app = Router::new().nest("/api", api_routes());

    let app = if let Some(bundle) = bundle {
        // If nothing else matched, always return the SPA. The client application has its own
        // router, which it will use to handle the requested the path.
        app.fallback_service(spa_service(&bundle))
    } else {
        // Make sure to include some information on why there is no UI showing.
        app.fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                "This instance of Eka CI has been started with the web interface disabled.",
            )
        })
    };

    axum::serve(listener, app).await.unwrap();
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

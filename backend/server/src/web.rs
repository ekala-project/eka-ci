use log::info;
use maud::html;
use warp::Filter;

const LOG_TARGET: &str = "eka-ci::server::web";

const BUNDLE: &[u8] = include_bytes!("../../../frontend/static/main.js");

fn bundle() -> &'static [u8] {
    BUNDLE
}

pub async fn serve_web(addr: std::net::Ipv4Addr, port: u16) {
    let root = warp::path::end().map(root);
    let bundle = warp::path("main.js").map(bundle);

    let routes = warp::get().and(bundle.or(root));

    info!(target: LOG_TARGET, "Serving Eka-CI on {}:{}", addr, port);

    warp::serve(routes).run((addr, port)).await
}

fn root() -> maud::Markup {
    html! {
        (maud::DOCTYPE)
        html lang="en" {
            head {
                // Some html boilerplate
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                style {
                    // Copied from `elm make` output, no idea why they do this and if we need it.
                    "body { padding: 0; margin: 0; }"
                }

                // Use a nice mono space font, because we deserve bettern than Courier.
                // No thought has been given to this choice, replace at will.
                link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/firacode@6.2.0/distr/fira_code.css";

                // The server has an additional endpoint configured that serves this file.
                // Empty body necessary to cirumvent https://github.com/lambda-fairy/maud/issues/474.
                script src="main.js" {}
            }
            body {
                script {
                    // No further setup required, straight into Elm we go.
                    "Elm.Main.init();"
                }
            }
        }
    }
}

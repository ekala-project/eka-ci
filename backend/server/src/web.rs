use warp::Filter;
use log::info;

const LOG_TARGET: &str = "eka-ci::server::web";

pub async fn serve_web(addr: std::net::Ipv4Addr, port: u16) {
    let about = warp::path("about")
    .map(|| format!("About Page"));

    // TODO: serve up frontend bundle
    let root = warp::path::end()
    .map(|| format!("Welcome to Eka-CI"));

    let routes = warp::get().and(about.or(root));

    info!(target: LOG_TARGET, "Serving Eka-CI on {}:{}", addr, port);

    warp::serve(routes)
    .run((addr, port))
    .await

}


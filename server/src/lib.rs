//! `webinarip serve`: a local web interface on 127.0.0.1 with the UI built into the binary.

mod api;
mod app;
mod assets;
mod files;
mod open;
mod records;
mod runner;
mod view;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::routing::{get, post};

pub struct ServeOptions {
    pub port: u16,
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub api_base: String,
    pub open_browser: bool,
}

pub async fn serve(o: ServeOptions) -> std::io::Result<()> {
    let app = app::App::new(o.out_dir, o.cache_dir, o.api_base);
    tokio::spawn(runner::run_queue(app.clone()));
    let router = Router::new()
        .route("/", get(assets::index))
        .route("/{file}", get(assets::file))
        .route("/api/probe", post(api::probe))
        .route("/api/jobs", post(api::jobs))
        .route("/api/state", get(api::state))
        .route("/api/reveal", post(api::reveal))
        .route("/api/jobs/{id}/cancel", post(api::cancel))
        .route("/api/jobs/{id}/audio", get(api::audio))
        .with_state(app);
    let listener = match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], o.port))).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?, // port busy: any free one
    };
    let url = format!("http://{}", listener.local_addr()?);
    println!("webinarip is running at {url} — press Ctrl+C to stop");
    if o.open_browser {
        open::browser(&url);
    }
    axum::serve(listener, router).await
}

//! The interface from `ui/`, compiled into the binary: no files to ship next to it.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

macro_rules! ui {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/", $name))
    };
}

const FILES: &[(&str, &str, &[u8])] = &[
    ("index.html", "text/html; charset=utf-8", ui!("index.html")),
    ("style.css", "text/css; charset=utf-8", ui!("style.css")),
    ("app.js", "text/javascript; charset=utf-8", ui!("app.js")),
    ("view.js", "text/javascript; charset=utf-8", ui!("view.js")),
    ("fmt.js", "text/javascript; charset=utf-8", ui!("fmt.js")),
];

fn serve(name: &str) -> Response {
    match FILES.iter().find(|f| f.0 == name) {
        Some((_, ctype, body)) => ([(header::CONTENT_TYPE, *ctype), (header::CACHE_CONTROL, "no-cache")], *body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn index() -> Response {
    serve("index.html")
}

pub async fn file(Path(name): Path<String>) -> Response {
    serve(&name)
}

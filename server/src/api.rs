//! HTTP handlers. Everything listens on 127.0.0.1 only.

use std::io::SeekFrom;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::app::{App, JobReq, Status};
use crate::view::{self, record_view};

type Shared = State<Arc<App>>;

fn fail(code: StatusCode, msg: impl ToString) -> Response {
    (code, Json(serde_json::json!({ "error": msg.to_string() }))).into_response()
}

#[derive(Deserialize)]
pub struct ProbeIn {
    link: String,
    session_id: Option<String>,
}

/// On paste: read the recording and start downloading its audio right away.
pub async fn probe(State(app): Shared, Json(p): Json<ProbeIn>) -> Response {
    let engine = match app.engine(p.session_id.as_deref()) {
        Ok(e) => e,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    let rec = match engine.record(p.link.trim()).await {
        Ok(r) => r,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    *app.probe.lock().unwrap() = Some((p.link.trim().to_owned(), rec.clone()));
    let (app2, rec2) = (app.clone(), rec.clone());
    tokio::spawn(async move {
        let _ = app2.download_for(&engine, &rec2).await; // speculative: ready before the user presses Enter
    });
    Json(record_view(&rec, None)).into_response()
}

#[derive(Deserialize)]
pub struct JobsIn {
    /// One or more links, separated by spaces or new lines.
    links: String,
    session_id: Option<String>,
    #[serde(flatten)]
    req: JobReq,
}

pub async fn jobs(State(app): Shared, Json(j): Json<JobsIn>) -> Response {
    let links: Vec<String> = j
        .links
        .split_whitespace()
        .filter(|l| l.contains("record-new/"))
        .map(str::to_owned)
        .collect();
    if links.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "no recording links (…/record-new/<id>)");
    }
    let ids: Vec<u64> = links
        .into_iter()
        .map(|l| app.enqueue(l, j.req.clone(), j.session_id.clone()))
        .collect();
    Json(serde_json::json!({ "ids": ids })).into_response()
}

pub async fn state(State(app): Shared) -> Response {
    Json(view::state(&app)).into_response()
}

pub async fn cancel(State(app): Shared, Path(id): Path<u64>) -> Response {
    let Some(job) = app.job(id) else {
        return fail(StatusCode::NOT_FOUND, "no such job");
    };
    job.cancel.store(true, Relaxed);
    let mut st = job.status.lock().unwrap();
    if *st == Status::Queued {
        *st = Status::Cancelled;
    } else if let Some(d) = job.download.lock().unwrap().as_ref() {
        d.cancel();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct RevealIn {
    path: String,
}

/// Shows a result in the file manager — only paths this app produced.
pub async fn reveal(State(app): Shared, Json(r): Json<RevealIn>) -> Response {
    let known = app.history.lock().unwrap().iter().any(|e| e.path == r.path)
        || app
            .jobs
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.path.lock().unwrap().as_ref().is_some_and(|p| p.display().to_string() == r.path));
    if !known {
        return fail(StatusCode::FORBIDDEN, "unknown path");
    }
    crate::open::reveal(std::path::Path::new(&r.path));
    StatusCode::NO_CONTENT.into_response()
}

fn content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("mp3") => "audio/mpeg",
        Some("opus") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        _ => "audio/wav",
    }
}

/// The job's audio: a growing stream while it renders, a seekable file once it is done.
pub async fn audio(State(app): Shared, Path(id): Path<u64>, headers: HeaderMap) -> Response {
    let Some(job) = app.job(id) else {
        return fail(StatusCode::NOT_FOUND, "no such job");
    };
    let Some(path) = job.path.lock().unwrap().clone() else {
        return fail(StatusCode::NOT_FOUND, "no audio yet");
    };
    let ctype = content_type(&path);
    if *job.status.lock().unwrap() == Status::Done {
        return file_range(&path, ctype, &headers).await;
    }
    let stream = futures_util::stream::unfold((None::<tokio::fs::File>, job), |(mut file, job)| async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if file.is_none() {
                let path = job.path.lock().unwrap().clone()?;
                file = tokio::fs::File::open(path).await.ok();
            }
            if let Some(f) = file.as_mut() {
                match f.read(&mut buf).await {
                    Ok(n) if n > 0 => return Some((Ok::<_, std::io::Error>(Bytes::copy_from_slice(&buf[..n])), (file, job))),
                    Err(e) => return Some((Err(e), (None, job))),
                    _ => {}
                }
            }
            if *job.status.lock().unwrap() != Status::Running {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    (
        [(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-store")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn file_range(path: &std::path::Path, ctype: &'static str, headers: &HeaderMap) -> Response {
    let Ok(mut f) = tokio::fs::File::open(path).await else {
        return fail(StatusCode::NOT_FOUND, "file is gone");
    };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="));
    let (start, end) = match range.and_then(|r| r.split_once('-')) {
        Some((a, b)) => (
            a.parse().unwrap_or(0),
            b.parse().unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1)),
        ),
        None => (0, len.saturating_sub(1)),
    };
    let mut body = vec![0u8; (end + 1).saturating_sub(start) as usize];
    if f.seek(SeekFrom::Start(start)).await.is_err() || f.read_exact(&mut body).await.is_err() {
        return fail(StatusCode::RANGE_NOT_SATISFIABLE, "bad range");
    }
    let code = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let cr = format!("bytes {start}-{end}/{len}");
    (
        code,
        [
            (header::CONTENT_TYPE, ctype),
            (header::ACCEPT_RANGES, "bytes"),
            (header::CONTENT_RANGE, cr.as_str()),
        ],
        body,
    )
        .into_response()
}

//! Finished files for the player: streamed from disk in 64 KB chunks with HTTP Range support
//! (seeking), never loaded into memory whole.

use std::io::SeekFrom;
use std::path::Path;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const CHUNK: usize = 64 * 1024;

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` → inclusive (start, end); `None` if unsatisfiable.
pub fn parse_range(header: Option<&str>, len: u64) -> Option<(u64, u64)> {
    let last = len.checked_sub(1)?;
    let Some(spec) = header.and_then(|h| h.strip_prefix("bytes=")) else {
        return Some((0, last));
    };
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim().parse::<u64>().ok(), b.trim().parse::<u64>().ok()) {
        (Some(s), Some(e)) => (s, e.min(last)),
        (Some(s), None) => (s, last),
        (None, Some(n)) => (len.saturating_sub(n), last),
        (None, None) => return None,
    };
    (start <= end).then_some((start, end))
}

pub async fn serve(path: &Path, ctype: &'static str, headers: &HeaderMap) -> Response {
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return (StatusCode::NOT_FOUND, "file is gone").into_response();
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let asked = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let Some((start, end)) = parse_range(asked, len) else {
        return (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}"))],
        )
            .into_response();
    };
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "bad range").into_response();
    }
    let stream = futures_util::stream::unfold((file, end - start + 1), |(mut f, left)| async move {
        if left == 0 {
            return None;
        }
        let mut buf = vec![0u8; CHUNK.min(left as usize)];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(Bytes::from(buf)), (f, left - n as u64)))
            }
            Err(e) => Some((Err(e), (f, 0))),
        }
    });
    let code = if asked.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = [
        (header::CONTENT_TYPE, ctype.to_owned()),
        (header::ACCEPT_RANGES, "bytes".to_owned()),
        (header::CONTENT_LENGTH, (end - start + 1).to_string()),
        (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
    ];
    (code, headers, Body::from_stream(stream)).into_response()
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 1000), Some((0, 999)));
        assert_eq!(parse_range(Some("bytes=100-199"), 1000), Some((100, 199)));
        assert_eq!(parse_range(Some("bytes=900-"), 1000), Some((900, 999)));
        assert_eq!(parse_range(Some("bytes=-100"), 1000), Some((900, 999)));
        assert_eq!(parse_range(Some("bytes=0-5000"), 1000), Some((0, 999)));
        assert_eq!(parse_range(Some("bytes=2000-"), 1000), None);
        assert_eq!(parse_range(None, 0), None);
    }
}

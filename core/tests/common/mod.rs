//! A mock recording server: metadata JSON, HLS playlists and fMP4 segments from
//! `tests/fixtures`, with a per-connection speed limit, injected 429s and one flaky segment.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;

#[derive(Default)]
pub struct Stats {
    pub conns_now: AtomicUsize,
    pub conns_max: AtomicUsize,
    pub segment_hits: AtomicUsize,
    pub sent_429: AtomicUsize,
}

pub struct Mock {
    pub addr: SocketAddr,
    pub stats: Arc<Stats>,
}

pub struct Config {
    /// Bytes per second on each connection.
    pub per_conn_rate: usize,
    /// The first N segment requests get `429 Too Many Requests`.
    pub first_429: usize,
    /// Answer the metadata request with 403 (private recording).
    pub private: bool,
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn record_json(base: &str) -> String {
    let session = |id: u64, conf: u64, at: f64, t: &str| {
        format!(
            r#"{{"module":"mediasession.add","relativeTime":{at},"data":{{"id":{id},"hlsUrl":"{base}/{t}/master.m3u8","stream":{{"conference":{{"id":{conf}}}}}}}}}"#
        )
    };
    format!(
        r#"{{"name":"Mock webinar","createAt":"2026-09-22T10:00:00+0300","duration":5.0,"user":{{"id":7}},"eventLogs":[
        {{"module":"conference.add","data":{{"id":1,"user":{{"id":7,"nickname":"Host"}}}}}},
        {{"module":"conference.add","data":{{"id":2,"user":{{"id":8,"nickname":"Guest"}}}}}},
        {},{},
        {{"module":"mediasession.update","data":{{"id":10,"duration":3.0}}}},
        {{"module":"mediasession.update","data":{{"id":11,"duration":3.0}}}}]}}"#,
        session(10, 1, 0.0, "t1"),
        session(11, 2, 2.0, "t2")
    )
}

async fn handle(
    req: Request<Incoming>,
    cfg: Arc<Config>,
    stats: Arc<Stats>,
    base: String,
    flaky: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path().to_owned();
    let reply = |code: u16, body: Vec<u8>| {
        Ok(Response::builder()
            .status(code)
            .header("retry-after", "0")
            .body(Full::new(Bytes::from(body)))
            .unwrap())
    };
    if path.starts_with("/api/eventsessions/") {
        return if cfg.private {
            reply(403, vec![])
        } else {
            reply(200, record_json(&base).into_bytes())
        };
    }
    if path.ends_with(".m4s") {
        stats.segment_hits.fetch_add(1, SeqCst);
        if stats.sent_429.load(SeqCst) < cfg.first_429 {
            stats.sent_429.fetch_add(1, SeqCst);
            return reply(429, vec![]);
        }
        if path == "/t2/a/seg1.m4s" && flaky.fetch_add(1, SeqCst) == 0 {
            return reply(500, vec![]); // fails once, must be retried
        }
    }
    match std::fs::read(fixtures().join(path.trim_start_matches('/'))) {
        Ok(body) => {
            tokio::time::sleep(Duration::from_secs_f64(body.len() as f64 / cfg.per_conn_rate as f64)).await;
            reply(200, body)
        }
        Err(_) => reply(404, vec![]),
    }
}

pub async fn start(cfg: Config) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stats, cfg, flaky) = (Arc::new(Stats::default()), Arc::new(cfg), Arc::new(AtomicUsize::new(0)));
    let base = format!("http://{addr}");
    let s = stats.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let now = s.conns_now.fetch_add(1, SeqCst) + 1;
            s.conns_max.fetch_max(now, SeqCst);
            let (cfg, s2, base, flaky) = (cfg.clone(), s.clone(), base.clone(), flaky.clone());
            tokio::spawn(async move {
                let counter = s2.clone();
                let svc = service_fn(move |req| handle(req, cfg.clone(), s2.clone(), base.clone(), flaky.clone()));
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(tcp), svc)
                    .await;
                counter.conns_now.fetch_sub(1, SeqCst);
            });
        }
    });
    Mock { addr, stats }
}

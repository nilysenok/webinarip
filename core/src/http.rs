//! HTTP/1.1-only client. HTTP/2 is deliberately off: the media server throttles per TCP
//! connection, and HTTP/2 squeezes every request into one connection (measured 7.7× slower).

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::header::{COOKIE, HeaderValue, RETRY_AFTER, USER_AGENT};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;

use crate::{Error, Result};

const UA: &str = concat!("webinarip/", env!("CARGO_PKG_VERSION"));

pub struct Response {
    pub status: u16,
    pub body: Bytes,
    pub retry_after: Option<Duration>,
}

pub struct Http {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
    /// Only ever lives in memory: never logged, never written to disk.
    cookie: Option<HeaderValue>,
}

impl Http {
    pub fn new(session_id: Option<&str>, max_idle: usize) -> Result<Self> {
        let mut tcp = HttpConnector::new();
        tcp.set_connect_timeout(Some(Duration::from_secs(10)));
        tcp.set_nodelay(true);
        tcp.enforce_http(false);
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .wrap_connector(tcp);
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(max_idle)
            .pool_idle_timeout(Duration::from_secs(30))
            .build(https);
        let cookie = session_id
            .map(|s| HeaderValue::from_str(&format!("sessionId={s}")))
            .transpose()
            .map_err(|_| Error::Usage("session id contains invalid characters".into()))?;
        Ok(Self { client, cookie })
    }

    /// GET with a whole-request deadline (headers and body).
    pub async fn get(&self, url: &str, deadline: Duration) -> Result<Response> {
        tokio::time::timeout(deadline, self.get_inner(url))
            .await
            .map_err(|_| Error::Net(format!("timeout after {}s", deadline.as_secs())))?
    }

    async fn get_inner(&self, url: &str) -> Result<Response> {
        let mut req = hyper::Request::get(url).header(USER_AGENT, UA);
        if let Some(c) = &self.cookie {
            req = req.header(COOKIE, c.clone());
        }
        let req = req.body(Empty::new()).map_err(|e| Error::Usage(format!("bad URL {url}: {e}")))?;
        let resp = self.client.request(req).await.map_err(|e| Error::Net(e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok()?.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = resp.into_body().collect().await.map_err(|e| Error::Net(e.to_string()))?.to_bytes();
        Ok(Response { status, body, retry_after })
    }

    /// GET that must succeed; maps auth failures to a helpful error.
    pub async fn get_ok(&self, url: &str) -> Result<Bytes> {
        let mut last = Error::Net("no attempts".into());
        for attempt in 0..4u32 {
            match self.get(url, Duration::from_secs(30)).await {
                Ok(r) if r.status == 200 => return Ok(r.body),
                Ok(r) if matches!(r.status, 401 | 403) => return Err(Error::Access(r.status)),
                Ok(r) if r.status == 404 => return Err(Error::Status(404, url.to_owned())),
                Ok(r) => last = Error::Status(r.status, url.to_owned()),
                Err(e) => last = e,
            }
            tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
        }
        Err(last)
    }
}

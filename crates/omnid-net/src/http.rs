use bytes::{Bytes, BytesMut};
use http::HeaderMap;
use http::header::{ACCEPT, CONNECTION, CONTENT_TYPE, HOST, HeaderName, LOCATION, USER_AGENT};
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use url::Url;

use crate::{BoxError, FetchOptions};

const ACCEPT_VALUE: &str = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1";

/// One HTTP exchange, before redirect handling.
pub(crate) struct RawResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Option<Bytes>,
    pub truncated: bool,
}

impl RawResponse {
    fn from_head(status: u16, headers: &HeaderMap) -> Self {
        Self {
            status,
            content_type: header(headers, CONTENT_TYPE),
            location: header(headers, LOCATION),
            body: None,
            truncated: false,
        }
    }

    fn wants_body(&self, opts: &FetchOptions) -> bool {
        !is_redirect(self.status) && (opts.accept)(self.content_type.as_deref())
    }
}

pub(crate) fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

pub(crate) async fn reqwest_get(
    client: &reqwest::Client,
    url: &str,
    opts: &FetchOptions,
) -> Result<RawResponse, reqwest::Error> {
    let mut response = client.get(url).header(ACCEPT, ACCEPT_VALUE).send().await?;
    let mut raw = RawResponse::from_head(response.status().as_u16(), response.headers());
    if raw.wants_body(opts) {
        let mut buf = BytesMut::new();
        while let Some(chunk) = response.chunk().await? {
            if push_capped(&mut buf, &chunk, opts.max_bytes) {
                raw.truncated = true;
                break;
            }
        }
        raw.body = Some(buf.freeze());
    }
    Ok(raw)
}

/// A single HTTP/1.1 GET over an already-open stream (a Tor circuit, or TLS
/// on top of one).
pub(crate) async fn http1_get<S>(
    stream: S,
    url: &Url,
    user_agent: &str,
    opts: &FetchOptions,
) -> Result<RawResponse, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    let connection = tokio::spawn(connection);
    let result = async {
        let host = url.host_str().ok_or("URL has no host")?;
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        let request = http::Request::get(&url[url::Position::BeforePath..url::Position::AfterQuery])
            .header(HOST, authority)
            .header(USER_AGENT, user_agent)
            .header(ACCEPT, ACCEPT_VALUE)
            .header(CONNECTION, "close")
            .body(Empty::<Bytes>::new())?;
        let response = sender.send_request(request).await?;
        let mut raw = RawResponse::from_head(response.status().as_u16(), response.headers());
        if raw.wants_body(opts) {
            let mut body = response.into_body();
            let mut buf = BytesMut::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame?.into_data()
                    && push_capped(&mut buf, &data, opts.max_bytes)
                {
                    raw.truncated = true;
                    break;
                }
            }
            raw.body = Some(buf.freeze());
        }
        Ok::<_, BoxError>(raw)
    }
    .await;
    connection.abort();
    result
}

/// Appends as much of `chunk` as fits under `max`; true once the cap is hit.
fn push_capped(buf: &mut BytesMut, chunk: &[u8], max: usize) -> bool {
    let room = max.saturating_sub(buf.len());
    if chunk.len() > room {
        buf.extend_from_slice(&chunk[..room]);
        true
    } else {
        buf.extend_from_slice(chunk);
        false
    }
}

fn header(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_body_size() {
        let mut buf = BytesMut::new();
        assert!(!push_capped(&mut buf, b"abc", 5));
        assert!(push_capped(&mut buf, b"defg", 5));
        assert_eq!(&buf[..], b"abcde");
    }
}

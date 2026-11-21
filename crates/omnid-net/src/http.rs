use std::io::Read;

use bytes::{Bytes, BytesMut};
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use http::HeaderMap;
use http::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, CONTENT_ENCODING, CONTENT_TYPE, HOST, HeaderName, LOCATION, USER_AGENT,
};
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
            // Tor bandwidth is the bottleneck; text compresses 4-10x.
            .header(ACCEPT_ENCODING, "gzip, deflate")
            .header(CONNECTION, "close")
            .body(Empty::<Bytes>::new())?;
        let response = sender.send_request(request).await?;
        let mut raw = RawResponse::from_head(response.status().as_u16(), response.headers());
        let encoding = header(response.headers(), CONTENT_ENCODING);
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
            let (body, truncated) = decode(encoding.as_deref(), buf.freeze(), raw.truncated, opts.max_bytes)?;
            raw.body = Some(body);
            raw.truncated = truncated;
        }
        Ok::<_, BoxError>(raw)
    }
    .await;
    connection.abort();
    result
}

/// Undoes `Content-Encoding`, never producing more than `max` bytes, so a
/// small compressed body cannot expand into gigabytes. A body cut short by
/// the download cap decodes as far as it goes.
fn decode(encoding: Option<&str>, body: Bytes, cut_short: bool, max: usize) -> Result<(Bytes, bool), BoxError> {
    let mut decoder: Box<dyn Read + '_> = match encoding.map(str::to_ascii_lowercase).as_deref() {
        None | Some("" | "identity") => return Ok((body, cut_short)),
        Some("gzip" | "x-gzip") => Box::new(GzDecoder::new(&body[..])),
        // "deflate" is meant to be zlib-wrapped, but some servers send raw deflate.
        Some("deflate") if is_zlib(&body) => Box::new(ZlibDecoder::new(&body[..])),
        Some("deflate") => Box::new(DeflateDecoder::new(&body[..])),
        Some(other) => return Err(format!("unsupported content encoding `{other}`").into()),
    };
    let mut out = Vec::new();
    match decoder.by_ref().take(max as u64 + 1).read_to_end(&mut out) {
        Ok(_) => {}
        // read_to_end keeps what it decoded before the error.
        Err(_) if cut_short && !out.is_empty() => {}
        Err(e) => return Err(e.into()),
    }
    let truncated = cut_short || out.len() > max;
    out.truncate(max);
    Ok((out.into(), truncated))
}

fn is_zlib(body: &[u8]) -> bool {
    matches!(body, [cmf, flg, ..] if cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0)
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
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

    use super::*;

    fn gzip(data: &[u8]) -> Bytes {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap().into()
    }

    #[test]
    fn decodes_gzip_and_both_deflates() {
        let text = b"<p>hello hello hello</p>".repeat(10);
        assert_eq!(decode(Some("gzip"), gzip(&text), false, 1 << 20).unwrap(), (Bytes::from(text.clone()), false));

        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        zlib.write_all(&text).unwrap();
        let mut raw = DeflateEncoder::new(Vec::new(), Compression::fast());
        raw.write_all(&text).unwrap();
        for body in [zlib.finish().unwrap(), raw.finish().unwrap()] {
            assert_eq!(decode(Some("Deflate"), body.into(), false, 1 << 20).unwrap().0, text);
        }
        assert_eq!(decode(None, Bytes::from_static(b"plain"), false, 10).unwrap().0, "plain");
        assert!(decode(Some("br"), Bytes::new(), false, 10).is_err());
    }

    #[test]
    fn caps_decompression_bombs() {
        let bomb = gzip(&vec![0u8; 20 << 20]);
        assert!(bomb.len() < 100 << 10);
        let (body, truncated) = decode(Some("gzip"), bomb, false, 1 << 20).unwrap();
        assert_eq!((body.len(), truncated), (1 << 20, true));
    }

    #[test]
    fn keeps_what_a_cut_short_body_decodes_to() {
        let text = b"0123456789".repeat(10_000);
        let compressed = gzip(&text);
        let half = compressed.slice(..compressed.len() / 2);
        let (body, truncated) = decode(Some("gzip"), half, true, 1 << 20).unwrap();
        assert!(truncated && !body.is_empty() && text.starts_with(&body));
        assert!(decode(Some("gzip"), compressed.slice(..10), false, 1 << 20).is_err());
    }

    #[test]
    fn caps_body_size() {
        let mut buf = BytesMut::new();
        assert!(!push_capped(&mut buf, b"abc", 5));
        assert!(push_capped(&mut buf, b"defg", 5));
        assert_eq!(&buf[..], b"abcde");
    }
}

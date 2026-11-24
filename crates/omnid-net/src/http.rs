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
    /// Whether the answer carries an IPFS gateway's headers: `Ipfs-Uri` or
    /// `X-Ipfs-Roots` (kubo 0.43 sends both), or the older `X-Ipfs-Path`.
    pub ipfs_gateway: bool,
    pub body: Option<Bytes>,
    pub truncated: bool,
}

impl RawResponse {
    fn from_head(status: u16, headers: &HeaderMap) -> Self {
        Self {
            status,
            content_type: header(headers, CONTENT_TYPE),
            location: header(headers, LOCATION),
            ipfs_gateway: ["ipfs-uri", "x-ipfs-roots", "x-ipfs-path"].iter().any(|name| headers.contains_key(*name)),
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

/// An HTTP/1.1 connection over an already-open stream (a Tor stream, or TLS
/// on one). It stays open as long as the sender lives.
pub(crate) type Sender = hyper::client::conn::http1::SendRequest<Empty<Bytes>>;

/// A redirect's or an error page's body is read and dropped, up to this, so
/// the connection can carry the next request.
const MAX_DRAIN: usize = 64 << 10;

pub(crate) async fn handshake<S>(stream: S) -> Result<Sender, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _closed = connection.await;
    });
    Ok(sender)
}

/// A GET on an open connection. Also says whether the connection can carry
/// another request: the response was read to its end and the server did not
/// ask to close.
pub(crate) async fn get(
    sender: &mut Sender,
    url: &Url,
    user_agent: &str,
    opts: &FetchOptions,
) -> Result<(RawResponse, bool), BoxError> {
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
        .body(Empty::<Bytes>::new())?;
    sender.ready().await?;
    let response = sender.send_request(request).await?;
    let mut raw = RawResponse::from_head(response.status().as_u16(), response.headers());
    let encoding = header(response.headers(), CONTENT_ENCODING);
    let close = header(response.headers(), CONNECTION).is_some_and(|c| c.eq_ignore_ascii_case("close"));
    let mut body = response.into_body();
    let mut complete = false;
    if raw.wants_body(opts) {
        let mut buf = BytesMut::new();
        loop {
            let Some(frame) = body.frame().await else {
                complete = true;
                break;
            };
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
    } else if is_redirect(raw.status) || raw.status >= 400 {
        // Small bodies only, and never a refused content type's: that one
        // must not be downloaded, so its connection is dropped instead.
        let mut drained = 0;
        loop {
            let Some(frame) = body.frame().await else {
                complete = true;
                break;
            };
            if let Ok(data) = frame?.into_data() {
                drained += data.len();
                if drained > MAX_DRAIN {
                    break;
                }
            }
        }
    }
    Ok((raw, complete && !close))
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
pub(crate) fn push_capped(buf: &mut BytesMut, chunk: &[u8], max: usize) -> bool {
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

/// For the fuzz targets in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A body under `Content-Encoding`, cut at the cap as [`get`] cuts one.
    /// `input`'s first byte picks the encoding, the next two the cap.
    pub fn decode(input: &[u8]) {
        let Some((&[pick, high, low], body)) = input.split_first_chunk() else { return };
        let encodings = [None, Some("gzip"), Some("X-Gzip"), Some("deflate"), Some("identity"), Some("br")];
        let encoding = encodings[usize::from(pick) % encodings.len()];
        let max = usize::from(u16::from_be_bytes([high, low]));
        let (body, cut_short) = (&body[..body.len().min(max)], body.len() > max);
        if let Ok((out, truncated)) = super::decode(encoding, Bytes::copy_from_slice(body), cut_short, max) {
            assert!(out.len() <= max, "{} bytes decoded past a cap of {max}", out.len());
            assert!(truncated || !cut_short);
            if matches!(encoding, None | Some("identity")) {
                assert_eq!(&out[..], body);
            }
        }
    }

    /// A server's answer to a GET on an open connection, as on a Tor stream.
    /// `input`'s first byte picks the cap and whether only text is wanted,
    /// the rest is what the server sends. A body is read only when its type
    /// is wanted, and never past the cap.
    pub fn response(input: &[u8]) {
        let Some((&pick, answer)) = input.split_first() else { return };
        let text: fn(Option<&str>) -> bool = |ct| ct.is_some_and(|ct| ct.starts_with("text/"));
        let opts = FetchOptions {
            max_bytes: [0, 16, 1 << 10, 64 << 10][usize::from(pick & 3)],
            accept: if pick & 4 == 0 { text } else { |_| true },
            max_redirects: 0,
        };
        let url = Url::parse("http://example.com/page").expect("a URL");
        // In memory, so without IO or a clock.
        let runtime = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
        let (client, mut server) = tokio::io::duplex(answer.len() + (64 << 10));
        let serve = async {
            // Answers once the request is in: a client takes bytes that come
            // before it asked for anything as an error.
            let mut request = Vec::new();
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                match server.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                }
            }
            server.write_all(answer).await.expect("room for the answer");
            server.shutdown().await.expect("an open stream");
        };
        let fetch = async {
            let mut sender = handshake(client).await?;
            get(&mut sender, &url, "omnid", &opts).await
        };
        let ((), got) = runtime.block_on(async { tokio::join!(serve, fetch) });
        if let Ok((raw, _)) = got {
            if let Some(body) = &raw.body {
                assert!(body.len() <= opts.max_bytes);
                assert!((opts.accept)(raw.content_type.as_deref()), "read a body of a type not wanted");
            }
            assert!(raw.body.is_some() || !raw.truncated);
        }
    }
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

    /// Serves pages that report the client's port, so a test can tell
    /// whether two requests shared a connection.
    async fn port_echo_server() -> std::net::SocketAddr {
        use axum::extract::ConnectInfo;
        use axum::http::header;
        use axum::response::{Html, IntoResponse};
        use axum::routing::get;

        let port = |ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>| async move { Html(peer.port().to_string()) };
        let app = axum::Router::new()
            .route("/page", get(port))
            .route("/big", get(|| async { Html("x".repeat(10_000)) }))
            .route(
                "/moved",
                get(|| async { (axum::http::StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/page")], "moved") }),
            )
            .route(
                "/image",
                get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![0u8; 10_000]).into_response() }),
            )
            .route("/close", get(|| async { ([(header::CONNECTION, "close")], Html("bye")) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap()
        });
        addr
    }

    #[tokio::test]
    async fn reuses_a_connection_only_when_the_response_was_read_to_its_end() {
        let addr = port_echo_server().await;
        let url = |path: &str| Url::parse(&format!("http://{addr}{path}")).unwrap();
        let opts = FetchOptions {
            max_bytes: 1_000,
            accept: |ct| ct.is_some_and(|ct| ct.starts_with("text/")),
            max_redirects: 0,
        };
        let connect = || async { handshake(tokio::net::TcpStream::connect(addr).await.unwrap()).await.unwrap() };
        async fn fetch(sender: &mut Sender, url: Url, opts: &FetchOptions) -> (RawResponse, bool) {
            get(sender, &url, "test", opts).await.unwrap()
        }

        let mut sender = connect().await;
        let (first, reusable) = fetch(&mut sender, url("/page"), &opts).await;
        assert!(reusable);
        let (redirect, reusable) = fetch(&mut sender, url("/moved"), &opts).await;
        assert_eq!((redirect.status, reusable), (301, true), "a redirect's small body is drained");
        let (second, reusable) = fetch(&mut sender, url("/page"), &opts).await;
        assert!(reusable);
        assert_eq!(first.body, second.body, "same client port: the connection was reused");

        let (big, reusable) = fetch(&mut sender, url("/big"), &opts).await;
        assert!(big.truncated && !reusable, "unread bytes are left on a truncated connection");

        let mut sender = connect().await;
        let (image, reusable) = fetch(&mut sender, url("/image"), &opts).await;
        assert!(image.body.is_none() && !reusable, "a refused body is not downloaded, so not drained");

        let mut sender = connect().await;
        let (_, reusable) = fetch(&mut sender, url("/close"), &opts).await;
        assert!(!reusable, "the server asked to close");
    }

    #[test]
    fn caps_body_size() {
        let mut buf = BytesMut::new();
        assert!(!push_capped(&mut buf, b"abc", 5));
        assert!(push_capped(&mut buf, b"defg", 5));
        assert_eq!(&buf[..], b"abcde");
    }
}

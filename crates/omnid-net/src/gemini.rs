//! Gemini (geminiprotocol.net): over TLS, the absolute URL as one line, a
//! header line back (`<status> <meta>`), then for a success the body until
//! the server closes the connection.

use std::sync::{Arc, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::TlsConnector;
use url::Url;

use crate::{BoxError, FetchOptions, SmallWeb, small_web};

/// A request is an absolute URL of at most this many bytes, then CRLF.
const MAX_REQUEST: usize = 1024;
/// A header: two digits, a space, a meta of at most 1024 bytes, CRLF.
const MAX_HEADER: usize = 2 + 1 + 1024 + 2;
/// What a success with an empty meta is.
const DEFAULT_TYPE: &str = "text/gemini; charset=utf-8";

/// Asks a Gemini server for `url` over `stream`, which reaches its host.
pub(crate) async fn get<S>(stream: S, url: &Url, opts: &FetchOptions) -> Result<SmallWeb, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let host = url.host_str().ok_or("a Gemini URL without a host")?;
    let name = ServerName::try_from(host.trim_start_matches('[').trim_end_matches(']').to_owned())?;
    let tls = TlsConnector::from(tls_config()?).connect(name, stream).await?;
    exchange(tls, url, opts).await
}

/// The request and its answer, over a connection that is already TLS.
async fn exchange<S>(mut stream: S, url: &Url, opts: &FetchOptions) -> Result<SmallWeb, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = format!("{url}\r\n");
    if request.len() > MAX_REQUEST + 2 {
        return Err("a Gemini request is at most 1024 bytes".into());
    }
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut line = Vec::new();
    while !line.ends_with(b"\n") {
        if line.len() >= MAX_HEADER {
            return Err("a Gemini header longer than 1029 bytes".into());
        }
        match stream.read_u8().await {
            Ok(byte) => line.push(byte),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !line.is_empty() => break,
            Err(e) => return Err(e.into()),
        }
    }
    let (status, meta) = parse_header(&line)?;
    Ok(match status / 10 {
        1 => SmallWeb::Input { prompt: meta, sensitive: status == 11 },
        2 => {
            let content_type = if meta.is_empty() { DEFAULT_TYPE.to_owned() } else { meta };
            let (body, truncated) = if (opts.accept)(Some(&content_type)) {
                let (body, truncated) = small_web::read_capped(&mut stream, opts.max_bytes).await?;
                (Some(body), truncated)
            } else {
                (None, false)
            };
            SmallWeb::Page { content_type, body, truncated }
        }
        3 => SmallWeb::Redirect { location: meta, permanent: status == 31 },
        _ => SmallWeb::Failed { status, message: meta },
    })
}

/// A header line: its status, two digits from 10 to 69, and its meta, at
/// most 1024 bytes of UTF-8. A bare LF ends a line too, and a meta may be
/// missing (`20\r\n`), as some servers send them.
pub(crate) fn parse_header(line: &[u8]) -> Result<(u8, String), BoxError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let (digits, rest) = line.split_at_checked(2).ok_or("a Gemini header without a status")?;
    let status = std::str::from_utf8(digits)
        .ok()
        .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|s| (10..70).contains(s))
        .ok_or("a Gemini header with no status from 10 to 69")?;
    let meta = match rest {
        [] => &[][..],
        [b' ' | b'\t', meta @ ..] => meta,
        _ => return Err("a Gemini status of more than two digits".into()),
    };
    if meta.len() > 1024 {
        return Err("a Gemini meta longer than 1024 bytes".into());
    }
    let meta = std::str::from_utf8(meta).map_err(|_| "a Gemini meta that is not UTF-8")?;
    Ok((status, meta.trim().to_owned()))
}

/// TLS 1.2 or 1.3 that takes any certificate: Geminispace runs on
/// self-signed ones, and clients trust a host's on first use, which a
/// crawler that keeps no state across hosts cannot. The handshake's
/// signature is still checked, so the server holds the key it shows.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, BoxError> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| Arc::new(config)).clone())
}

#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(fuzzing)]
pub(crate) mod fuzz {
    /// Gemini's header line parser, for `fuzz/fuzz_targets/gemini_header.rs`.
    pub fn header(data: &[u8]) {
        if let Ok((status, meta)) = super::parse_header(data) {
            assert!((10..70).contains(&status));
            assert!(meta.len() <= 1024);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_header_lines() {
        assert_eq!(parse_header(b"20 text/gemini\r\n").unwrap(), (20, "text/gemini".into()));
        assert_eq!(parse_header(b"20\r\n").unwrap(), (20, String::new()), "an empty meta");
        assert_eq!(parse_header(b"31 gemini://a.example/new\n").unwrap(), (31, "gemini://a.example/new".into()));
        assert_eq!(parse_header(b"10 Search terms?\r\n").unwrap(), (10, "Search terms?".into()));
        for bad in [&b"2 text/gemini\r\n"[..], b"200 OK\r\n", b"xx meta\r\n", b"70 meta\r\n", b"09 meta\r\n", b""] {
            assert!(parse_header(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let long = format!("20 {}\r\n", "a".repeat(1025));
        assert!(parse_header(long.as_bytes()).is_err(), "a meta over 1024 bytes");
        assert!(parse_header(b"20 \xff\r\n").is_err(), "not UTF-8");
    }
    /// A capsule on the other end of an in-memory connection, TLS aside:
    /// it reads the request line and answers with `answer`.
    async fn ask(answer: &'static [u8], opts: FetchOptions) -> (Result<SmallWeb, String>, String) {
        let (client, mut server) = tokio::io::duplex(64 << 10);
        let capsule = tokio::spawn(async move {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n") {
                request.push(server.read_u8().await.unwrap());
            }
            server.write_all(answer).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        let url = Url::parse("gemini://a.example/log/").unwrap();
        let answer = exchange(client, &url, &opts).await.map_err(|e| e.to_string());
        (answer, capsule.await.unwrap())
    }

    #[tokio::test]
    async fn asks_for_a_page_and_reads_the_answer() {
        let (answer, request) = ask(b"20 text/gemini; lang=en\r\n# Log\n", FetchOptions::default()).await;
        assert_eq!(request, "gemini://a.example/log/\r\n");
        assert_eq!(
            answer.unwrap(),
            SmallWeb::Page {
                content_type: "text/gemini; lang=en".into(),
                body: Some(bytes::Bytes::from_static(b"# Log\n")),
                truncated: false
            }
        );
        let (answer, _) = ask(b"20\r\nhello", FetchOptions::default()).await;
        assert!(matches!(answer.unwrap(), SmallWeb::Page { content_type, .. } if content_type == DEFAULT_TYPE));
    }

    #[tokio::test]
    async fn leaves_refused_types_unread_and_caps_bodies() {
        let refuse = FetchOptions { accept: |_| false, ..FetchOptions::default() };
        let (answer, _) = ask(b"20 image/png\r\n\x89PNG", refuse).await;
        assert!(matches!(answer.unwrap(), SmallWeb::Page { body: None, .. }));
        let small = FetchOptions { max_bytes: 4, ..FetchOptions::default() };
        let (answer, _) = ask(b"20 text/plain\r\nlonger than four", small).await;
        assert!(
            matches!(answer.unwrap(), SmallWeb::Page { body: Some(b), truncated: true, .. } if b.as_ref() == b"long")
        );
    }

    #[tokio::test]
    async fn reads_input_redirects_and_failures() {
        let (answer, _) = ask(b"11 Password\r\n", FetchOptions::default()).await;
        assert_eq!(answer.unwrap(), SmallWeb::Input { prompt: "Password".into(), sensitive: true });
        let (answer, _) = ask(b"31 /new/\r\n", FetchOptions::default()).await;
        assert_eq!(answer.unwrap(), SmallWeb::Redirect { location: "/new/".into(), permanent: true });
        let (answer, _) = ask(b"51 Not found\r\n", FetchOptions::default()).await;
        assert_eq!(answer.unwrap(), SmallWeb::Failed { status: 51, message: "Not found".into() });
        let endless: &'static [u8] = Box::leak(vec![b'2'; 4096].into_boxed_slice());
        let (answer, _) = ask(endless, FetchOptions::default()).await;
        assert!(answer.unwrap_err().contains("longer than 1029"), "a header without an end");
    }
}

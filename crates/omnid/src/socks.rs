//! The proxy in front of the gateway: SOCKS5 (RFC 1928) and HTTP on one
//! port. Point a browser at it with remote DNS ("socks5h"), or an app that
//! only speaks HTTP proxies at the same address, and `.onion` and `.i2p`
//! addresses work like any other.
//!
//! [`Access::Open`] serves `Gateway::connect`: the clearnet goes direct, or
//! through Tor with `clearnet_via_tor`. [`Access::Browser`] serves the app's
//! own browser through `Gateway::connect_browser`: only clients with the
//! password get in, everything goes through Tor (or i2pd), and the username,
//! the site the browser is on, keeps each site on circuits of its own.
//! [`Access::Apps`] serves other apps the same way, never direct, with each
//! username on circuits of its own, apart from the browser's sites.
//!
//! Over HTTP, CONNECT opens a tunnel and an absolute-form request
//! (`GET http://site/ HTTP/1.1`) is passed on in origin form, one per
//! connection; `Proxy-Authorization: Basic` carries the credentials.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use omnid_net::{FetchError, Gateway};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use url::Url;

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0x00;
const USER_PASS: u8 = 0x02;
const NO_ACCEPTABLE_METHOD: u8 = 0xff;
const CONNECT: u8 = 0x01;

const SUCCEEDED: u8 = 0x00;
const GENERAL_FAILURE: u8 = 0x01;
const NOT_ALLOWED: u8 = 0x02;
const HOST_UNREACHABLE: u8 = 0x04;
const TTL_EXPIRED: u8 = 0x06;
const COMMAND_NOT_SUPPORTED: u8 = 0x07;
const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;

/// A client must have said where it is going by now: one that connects and
/// says nothing does not keep a task.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Clients one listener serves at once. Past it, a new connection is closed
/// at once, so no app that can reach the port can tie the daemon up.
const MAX_CLIENTS: usize = 512;
/// An HTTP request's head: its request line and headers.
const MAX_HTTP_HEAD: usize = 16 << 10;
/// The realm in a 407's `Proxy-Authenticate`.
const REALM: &str = "OMNId";

/// Who may use a listener, and how its streams are routed.
pub enum Access {
    /// Anyone who can reach it; credentials are accepted and ignored.
    Open,
    /// Only clients that send this password (RFC 1929). The username names the
    /// site the browser is on. SOCKS only.
    Browser { password: String },
    /// Other apps: never direct, as for the browser, and each username on
    /// circuits of its own. With a password, only clients that send it get in.
    Apps { password: Option<String> },
}

impl Access {
    fn password(&self) -> Option<&str> {
        match self {
            Access::Open => None,
            Access::Browser { password } => Some(password),
            Access::Apps { password } => password.as_deref(),
        }
    }
}

/// A request that passed the handshake: where to, who asked, and how to
/// answer once the stream is open.
struct Request {
    host: String,
    port: u16,
    username: Option<String>,
    reply: Reply,
    /// Bytes the client sent after its request head; they go upstream first.
    early: Vec<u8>,
}

enum Reply {
    Socks,
    /// HTTP CONNECT: a 200, then the tunnel.
    HttpTunnel,
    /// An HTTP request to pass on: this head, rewritten to origin form.
    HttpForward(Vec<u8>),
}

pub async fn serve(listener: TcpListener, gateway: Arc<Gateway>, access: Arc<Access>, shutdown: CancellationToken) {
    serve_with(listener, gateway, access, shutdown, MAX_CLIENTS, HANDSHAKE_TIMEOUT).await;
}

async fn serve_with(
    listener: TcpListener,
    gateway: Arc<Gateway>,
    access: Arc<Access>,
    shutdown: CancellationToken,
    max_clients: usize,
    handshake_timeout: Duration,
) {
    let slots = Arc::new(Semaphore::new(max_clients));
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((client, _)) => {
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    tracing::debug!("proxy: {max_clients} clients already; closing a new one");
                    continue;
                };
                let (gateway, access) = (gateway.clone(), access.clone());
                tokio::spawn(async move {
                    let _slot = slot;
                    if let Err(e) = proxy(client, &gateway, &access, handshake_timeout).await {
                        tracing::debug!("proxy: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("proxy: accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn proxy<S: AsyncRead + AsyncWrite + Unpin>(
    mut client: S,
    gateway: &Gateway,
    access: &Access,
    handshake_timeout: Duration,
) -> io::Result<()> {
    let request = tokio::time::timeout(handshake_timeout, handshake(&mut client, access))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the client said nothing in time"))??;
    let Request { host, port, username, reply, early } = request;
    let username = username.unwrap_or_default();
    // Taken before connecting: a New identity from now on ends this stream.
    let identity = gateway.identity();
    let connected = match access {
        Access::Open => gateway.connect(&host, port).await,
        Access::Browser { .. } => gateway.connect_browser(&host, port, &username).await,
        // A host name never has a colon, so these keys never meet a site's.
        Access::Apps { .. } => gateway.connect_browser(&host, port, &format!("app:{username}")).await,
    };
    match connected {
        Ok(mut upstream) => {
            match &reply {
                Reply::Socks => reply_socks(&mut client, SUCCEEDED).await?,
                Reply::HttpTunnel => client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?,
                Reply::HttpForward(head) => upstream.write_all(head).await?,
            }
            upstream.write_all(&early).await?;
            // Tor streams buffer writes until flushed.
            upstream.flush().await?;
            let browser = matches!(access, Access::Browser { .. });
            tokio::select! {
                copied = tokio::io::copy_bidirectional(&mut client, &mut upstream) => copied.map(|_| ()),
                // The browser's New identity: its streams on the old circuits end.
                () = identity.cancelled(), if browser => Ok(()),
            }
        }
        Err(e) => {
            match reply {
                Reply::Socks => reply_socks(&mut client, reply_code(&e)).await?,
                Reply::HttpTunnel | Reply::HttpForward(_) => {
                    let (status, reason) = http_status(&e);
                    reply_http(&mut client, status, reason, &e.to_string()).await?;
                }
            }
            Err(io::Error::other(format!("{host}:{port}: {e}")))
        }
    }
}

/// Reads the first byte to tell SOCKS5 from HTTP, then the rest of the
/// request in that protocol.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, access: &Access) -> io::Result<Request> {
    let [first] = read_array(s).await?;
    match first {
        VERSION => socks_handshake(s, access).await,
        b'A'..=b'Z' if !matches!(access, Access::Browser { .. }) => http_handshake(s, first, access).await,
        _ => Err(invalid("neither SOCKS5 nor HTTP")),
    }
}

/// Negotiates a method, checks credentials if `access` needs them, and reads
/// a CONNECT request. The version byte has been read.
async fn socks_handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, access: &Access) -> io::Result<Request> {
    let [count] = read_array(s).await?;
    let mut methods = vec![0u8; count.into()];
    s.read_exact(&mut methods).await?;
    // Without a password, credentials are taken and only name the client's
    // circuits, so clients that always send them (to isolate their streams
    // in Tor) still work.
    let acceptable: &[u8] = match access.password() {
        None => &[NO_AUTH, USER_PASS],
        Some(_) => &[USER_PASS],
    };
    let method = acceptable.iter().copied().find(|m| methods.contains(m));
    let Some(method) = method else {
        s.write_all(&[VERSION, NO_ACCEPTABLE_METHOD]).await?;
        return Err(invalid("no acceptable authentication method"));
    };
    s.write_all(&[VERSION, method]).await?;
    let mut username = None;
    if method == USER_PASS {
        // RFC 1929: VER ULEN UNAME PLEN PASSWD; the reply is VER STATUS.
        let [_, user_len] = read_array(s).await?;
        let mut user = vec![0u8; user_len.into()];
        s.read_exact(&mut user).await?;
        let [pass_len] = read_array(s).await?;
        let mut pass = vec![0u8; pass_len.into()];
        s.read_exact(&mut pass).await?;
        if let Some(password) = access.password()
            && !crate::api::constant_time_eq(&pass, password.as_bytes())
        {
            s.write_all(&[1, 1]).await?;
            return Err(invalid("wrong password"));
        }
        s.write_all(&[1, 0]).await?;
        username = Some(String::from_utf8_lossy(&user).into_owned());
    }

    let [_, command, _, address_type] = read_array(s).await?;
    if command != CONNECT {
        reply_socks(s, COMMAND_NOT_SUPPORTED).await?;
        return Err(invalid("only CONNECT is supported"));
    }
    let host = match address_type {
        0x01 => Ipv4Addr::from(read_array::<4, _>(s).await?).to_string(),
        0x03 => {
            let [len] = read_array(s).await?;
            let mut name = vec![0u8; len.into()];
            s.read_exact(&mut name).await?;
            match String::from_utf8(name) {
                Ok(name) if valid_host(&name) => name,
                _ => {
                    reply_socks(s, NOT_ALLOWED).await?;
                    return Err(invalid("not a host name"));
                }
            }
        }
        0x04 => Ipv6Addr::from(read_array::<16, _>(s).await?).to_string(),
        _ => {
            reply_socks(s, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(invalid("unknown address type"));
        }
    };
    let port = u16::from_be_bytes(read_array(s).await?);
    Ok(Request { host, port, username, reply: Reply::Socks, early: Vec::new() })
}

/// Reads an HTTP request head that began with `first`, checks the password
/// if `access` has one, and turns the head into a [`Request`]: CONNECT into a
/// tunnel, an absolute-form `http://` request into one to pass on.
async fn http_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    first: u8,
    access: &Access,
) -> io::Result<Request> {
    let mut buf = vec![first];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HTTP_HEAD {
            reply_http(s, 431, "Request Header Fields Too Large", "the request head is too large").await?;
            return Err(invalid("oversized request head"));
        }
        let mut chunk = [0u8; 2048];
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let early = buf.split_off(end);
    let Some(head) = parse_head(&buf) else {
        reply_http(s, 400, "Bad Request", "not an HTTP/1 proxy request").await?;
        return Err(invalid("malformed request head"));
    };
    let credentials = head.header("proxy-authorization").and_then(basic_credentials);
    let username = match (access.password(), credentials) {
        (None, credentials) => credentials.map(|(user, _)| user),
        (Some(password), Some((user, pass))) if crate::api::constant_time_eq(pass.as_bytes(), password.as_bytes()) => {
            Some(user)
        }
        (Some(_), _) => {
            let challenge = format!(
                "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"{REALM}\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            s.write_all(challenge.as_bytes()).await?;
            return Err(invalid("wrong or missing proxy password"));
        }
    };
    if head.method == "CONNECT" {
        let Some((host, port)) = authority(head.target) else {
            reply_http(s, 400, "Bad Request", "CONNECT wants host:port").await?;
            return Err(invalid("bad CONNECT target"));
        };
        return Ok(Request { host, port, username, reply: Reply::HttpTunnel, early });
    }
    let url = Url::parse(head.target).ok().filter(|u| u.scheme() == "http" && u.host_str().is_some());
    let Some(url) = url else {
        reply_http(s, 400, "Bad Request", "only http:// addresses in absolute form; use CONNECT for https").await?;
        return Err(invalid("not an absolute http:// target"));
    };
    let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_owned();
    // The URL parser takes names, such as `)`, that no router should see.
    if !valid_host(&host) {
        reply_http(s, 400, "Bad Request", "not a host name").await?;
        return Err(invalid("not a host name"));
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let mut forward =
        format!("{} {} {}\r\n", head.method, &url[url::Position::BeforePath..url::Position::AfterQuery], head.version);
    for (name, value) in &head.headers {
        let hop_by_hop = ["proxy-authorization", "proxy-connection", "connection", "keep-alive"];
        if !hop_by_hop.iter().any(|h| name.eq_ignore_ascii_case(h)) {
            forward.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    // One request per connection: the next may be for another site.
    forward.push_str("Connection: close\r\n\r\n");
    Ok(Request { host, port, username, reply: Reply::HttpForward(forward.into_bytes()), early })
}

struct Head<'a> {
    method: &'a str,
    target: &'a str,
    version: &'a str,
    headers: Vec<(&'a str, &'a str)>,
}

impl Head<'_> {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }
}

/// A request head, strictly: one request line of three parts, then
/// `name: value` lines, none holding a lone CR, LF or NUL, and no folded
/// lines. Anything else is refused rather than passed on, where a server
/// might read it differently.
fn parse_head(head: &[u8]) -> Option<Head<'_>> {
    let head = std::str::from_utf8(head).ok()?.strip_suffix("\r\n\r\n")?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let token =
        |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    if parts.next().is_some() || !token(method) || target.is_empty() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if !token(name) || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return None;
        }
        headers.push((name, value.trim_matches(|c| c == ' ' || c == '\t')));
    }
    Some(Head { method, target, version, headers })
}

/// `user` and `password` from `Basic base64(user:password)`.
fn basic_credentials(value: &str) -> Option<(String, String)> {
    let (scheme, encoded) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(base64_decode(encoded.trim())?).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_owned(), password.to_owned()))
}

/// Standard base64 with padding (RFC 4648 §4); `None` for anything else.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (i, quad) in bytes.chunks(4).enumerate() {
        let last = i == bytes.len() / 4 - 1;
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            n = n << 6 | u32::from(value(c)?);
        }
        n <<= 6 * pad as u32;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

/// `host:port` or `[v6]:port`, as CONNECT names its target.
fn authority(target: &str) -> Option<(String, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if !valid_host(host) {
        return None;
    }
    Some((host.to_owned(), port.parse().ok()?))
}

/// A host a client may ask for: an IP address, or a name of letters, digits,
/// `-`, `_` and dots. Nothing else reaches a router, where a CR or LF would
/// let the client write its own request lines into the CONNECT sent to i2pd.
fn valid_host(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
        || (!host.is_empty()
            && host.len() <= 253
            && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
}

/// The bound address is not meaningful here, so it is reported as 0.0.0.0:0.
async fn reply_socks<S: AsyncWrite + Unpin>(s: &mut S, code: u8) -> io::Result<()> {
    s.write_all(&[VERSION, code, 0, 0x01, 0, 0, 0, 0, 0, 0]).await
}

async fn reply_http<S: AsyncWrite + Unpin>(s: &mut S, status: u16, reason: &str, detail: &str) -> io::Result<()> {
    let body = format!("{detail}\n");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(response.as_bytes()).await
}

fn reply_code(error: &FetchError) -> u8 {
    match error {
        FetchError::PrivateAddress(_) => NOT_ALLOWED,
        FetchError::Timeout(_) => TTL_EXPIRED,
        FetchError::Unavailable { .. } => GENERAL_FAILURE,
        _ => HOST_UNREACHABLE,
    }
}

fn http_status(error: &FetchError) -> (u16, &'static str) {
    match error {
        FetchError::PrivateAddress(_) => (403, "Forbidden"),
        FetchError::Timeout(_) => (504, "Gateway Timeout"),
        FetchError::Unavailable { .. } => (503, "Service Unavailable"),
        _ => (502, "Bad Gateway"),
    }
}

async fn read_array<const N: usize, S: AsyncRead + Unpin>(s: &mut S) -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// For the fuzz target in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    use super::*;

    /// A client's first bytes, as [`handshake`] reads them. `input`'s first
    /// byte picks who may use the listener; the rest is what the client
    /// sends. A request that passes names a host a router can be asked for,
    /// was let in only with the password where there is one, and is passed
    /// on as a head that parses as strictly as the client's had to.
    pub fn handshake(input: &[u8]) {
        let Some((&pick, sent)) = input.split_first() else { return };
        let password = || "s3cret".to_owned();
        let access = match pick % 4 {
            0 => Access::Open,
            1 => Access::Browser { password: password() },
            2 => Access::Apps { password: None },
            _ => Access::Apps { password: Some(password()) },
        };
        let runtime = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
        let (mut client, mut server) = tokio::io::duplex(sent.len() + MAX_HTTP_HEAD);
        let Ok(request) = runtime.block_on(async {
            client.write_all(sent).await.expect("room for the request");
            client.shutdown().await.expect("an open stream");
            super::handshake(&mut server, &access).await
        }) else {
            return;
        };
        assert!(valid_host(&request.host), "{:?}", request.host);
        assert!(access.password().is_none() || request.username.is_some(), "let in without the password");
        if let Reply::HttpForward(head) = &request.reply {
            let head = parse_head(head).expect("a forwarded head that parses");
            assert!(head.target.starts_with('/'), "{:?}", head.target);
            assert!(head.header("proxy-authorization").is_none());
            assert_eq!(head.headers.last(), Some(&("Connection", "close")));
        }
    }
}

#[cfg(test)]
mod tests {
    use omnid_net::GatewayConfig;

    use super::*;

    async fn negotiate_with(access: &Access, request: &[u8]) -> (io::Result<(String, u16, Option<String>)>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(1024);
        client.write_all(request).await.unwrap();
        let result = handshake(&mut server, access).await.map(|r| (r.host, r.port, r.username));
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    async fn negotiate(request: &[u8]) -> (io::Result<(String, u16)>, Vec<u8>) {
        let (result, written) = negotiate_with(&Access::Open, request).await;
        (result.map(|(host, port, _)| (host, port)), written)
    }

    fn browser() -> Access {
        Access::Browser { password: "s3cret".into() }
    }

    #[tokio::test]
    async fn reads_domain_ipv4_and_ipv6_targets() {
        let (target, written) = negotiate(b"\x05\x01\x00\x05\x01\x00\x03\x09stats.i2p\x00\x50").await;
        assert_eq!(target.unwrap(), ("stats.i2p".into(), 80));
        assert_eq!(written, [5, 0], "chose no authentication");

        let (target, _) = negotiate(b"\x05\x01\x00\x05\x01\x00\x01\x7f\x00\x00\x01\x01\xbb").await;
        assert_eq!(target.unwrap(), ("127.0.0.1".into(), 443));

        let mut v6 = b"\x05\x01\x00\x05\x01\x00\x04".to_vec();
        v6.extend(Ipv6Addr::LOCALHOST.octets());
        v6.extend(8080u16.to_be_bytes());
        assert_eq!(negotiate(&v6).await.0.unwrap(), ("::1".into(), 8080));
    }

    #[tokio::test]
    async fn accepts_and_ignores_credentials() {
        let (target, written) = negotiate(b"\x05\x01\x02\x01\x04user\x04pass\x05\x01\x00\x03\x01a\x00\x50").await;
        assert_eq!(target.unwrap(), ("a".into(), 80));
        assert_eq!(written, [5, 2, 1, 0]);
    }

    #[tokio::test]
    async fn refuses_what_it_does_not_speak() {
        let (target, written) = negotiate(b"\x05\x01\x01").await;
        assert!(target.is_err());
        assert_eq!(written, [5, 0xff], "GSSAPI only");

        let (target, written) = negotiate(b"\x05\x01\x00\x05\x02\x00\x03\x01a\x00\x50").await;
        assert!(target.is_err(), "BIND");
        assert_eq!(written[2..4], [5, COMMAND_NOT_SUPPORTED]);

        assert!(negotiate(b"\x04\x01\x00\x50").await.0.is_err(), "SOCKS4");
    }

    #[tokio::test]
    async fn proxies_a_connection_end_to_end() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let (mut read, mut write) = stream.split();
            tokio::io::copy(&mut read, &mut write).await.unwrap();
        });
        let gateway =
            Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve(listener, Arc::new(gateway), Arc::new(Access::Open), shutdown.clone()));

        let mut client = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let mut request = b"\x05\x01\x00\x05\x01\x00\x03\x09localhost".to_vec();
        request.extend(echo_port.to_be_bytes());
        client.write_all(&request).await.unwrap();
        let mut replies = [0u8; 12];
        client.read_exact(&mut replies).await.unwrap();
        assert_eq!(replies[..4], [5, 0, 5, SUCCEEDED]);
        client.write_all(b"through the proxy").await.unwrap();
        let mut echoed = [0u8; 17];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"through the proxy");
        shutdown.cancel();
    }

    #[tokio::test]
    async fn reports_refusals_with_socks_codes() {
        let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
        let (mut client, server) = tokio::io::duplex(1024);
        client.write_all(b"\x05\x01\x00\x05\x01\x00\x01\x7f\x00\x00\x01\x00\x50").await.unwrap();
        assert!(proxy(server, &gateway, &Access::Open, HANDSHAKE_TIMEOUT).await.is_err());
        let mut replies = [0u8; 12];
        client.read_exact(&mut replies).await.unwrap();
        assert_eq!(replies[3], NOT_ALLOWED, "loopback is refused by default");
    }

    #[tokio::test]
    async fn the_browser_listener_wants_the_password() {
        let (target, written) = negotiate_with(&browser(), b"\x05\x01\x00").await;
        assert!(target.is_err());
        assert_eq!(written, [5, 0xff], "no authentication is not offered");

        let (target, written) = negotiate_with(&browser(), b"\x05\x01\x02\x01\x0bexample.com\x06s3cres").await;
        assert!(target.is_err());
        assert_eq!(written, [5, 2, 1, 1], "a wrong password fails");

        let mut right = b"\x05\x02\x00\x02\x01\x0bexample.com\x06s3cret".to_vec();
        right.extend(b"\x05\x01\x00\x03\x0bcdn.example\x01\xbb");
        let (target, written) = negotiate_with(&browser(), &right).await;
        assert_eq!(
            target.unwrap(),
            ("cdn.example".into(), 443, Some("example.com".into())),
            "the username is the site"
        );
        assert_eq!(written, [5, 2, 1, 0]);
    }

    #[tokio::test]
    async fn the_browser_listener_never_goes_direct() {
        // Direct connections to loopback are allowed and Tor is off: the
        // browser's request must fail instead of reaching the server.
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = target.local_addr().unwrap().port();
        let gateway =
            Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
                .unwrap();
        let (mut client, server) = tokio::io::duplex(1024);
        let mut request = b"\x05\x01\x02\x01\x0bexample.com\x06s3cret\x05\x01\x00\x01\x7f\x00\x00\x01".to_vec();
        request.extend(port.to_be_bytes());
        client.write_all(&request).await.unwrap();
        assert!(proxy(server, &gateway, &browser(), HANDSHAKE_TIMEOUT).await.is_err());
        let mut replies = [0u8; 14];
        client.read_exact(&mut replies).await.unwrap();
        assert_eq!(replies[4..6], [5, GENERAL_FAILURE], "Tor is off, so nothing is reachable");
        let accepted = tokio::time::timeout(std::time::Duration::from_millis(200), target.accept()).await;
        assert!(accepted.is_err(), "nothing connected to the server");
    }

    fn apps(password: Option<&str>) -> Access {
        Access::Apps { password: password.map(str::to_owned) }
    }

    fn local_gateway() -> Gateway {
        Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
            .unwrap()
    }

    /// Sends `request` to a proxy for `access`; returns how the proxy ended and
    /// everything it answered.
    async fn exchange(gateway: &Gateway, access: &Access, request: &[u8]) -> (io::Result<()>, String) {
        let (mut client, server) = tokio::io::duplex(64 << 10);
        client.write_all(request).await.unwrap();
        let result = proxy(server, gateway, access, HANDSHAKE_TIMEOUT).await;
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).await.unwrap();
        (result, String::from_utf8_lossy(&answer).into_owned())
    }

    async fn listen(access: Access) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, Arc::new(local_gateway()), Arc::new(access), CancellationToken::new()));
        addr
    }

    #[tokio::test]
    async fn tunnels_http_connect_end_to_end() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let (mut read, mut write) = stream.split();
            tokio::io::copy(&mut read, &mut write).await.unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(listen(Access::Open).await).await.unwrap();
        client
            .write_all(format!("CONNECT localhost:{echo_port} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut answer = [0u8; 39];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"HTTP/1.1 200 Connection established\r\n\r\n");
        client.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
    }

    #[tokio::test]
    async fn passes_http_requests_on_in_origin_form() {
        let site = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site_port = site.local_addr().unwrap().port();
        let seen = tokio::spawn(async move {
            let (mut stream, _) = site.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await.unwrap();
            String::from_utf8(head).unwrap()
        });
        let mut client = tokio::net::TcpStream::connect(listen(Access::Open).await).await.unwrap();
        let request = format!(
            "GET http://localhost:{site_port}/path?q=1 HTTP/1.1\r\nHost: localhost\r\nProxy-Authorization: Basic dTpw\r\n\
             Proxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n"
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).await.unwrap();
        assert!(answer.ends_with("\r\n\r\nok"), "{answer}");
        let head = seen.await.unwrap();
        assert!(head.starts_with("GET /path?q=1 HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Accept: */*\r\n") && head.ends_with("Connection: close\r\n\r\n"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("proxy-"), "the proxy's own headers stay here: {head}");
    }

    #[tokio::test]
    async fn other_apps_need_the_password_in_either_protocol() {
        let gateway = local_gateway();
        let private = apps(Some("s3cret"));
        let (result, answer) = exchange(&gateway, &private, b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
        assert!(result.is_err());
        assert!(
            answer.starts_with("HTTP/1.1 407 ") && answer.contains("Proxy-Authenticate: Basic realm=\"OMNId\""),
            "{answer}"
        );
        // "app:wrong"
        let wrong = b"CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic YXBwOndyb25n\r\n\r\n";
        assert!(exchange(&gateway, &private, wrong).await.1.starts_with("HTTP/1.1 407 "));
        let (result, written) = negotiate_with(&private, b"\x05\x01\x00").await;
        assert!(result.is_err());
        assert_eq!(written, [5, 0xff], "SOCKS without credentials is not offered");
    }

    #[tokio::test]
    async fn other_apps_never_go_direct() {
        // Direct connections to loopback would be allowed and Tor is off: the
        // request must fail without reaching the server, password or not.
        let (port, accepted) = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = accepted.clone();
            tokio::spawn(async move {
                while listener.accept().await.is_ok() {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
            (port, accepted)
        };
        let gateway = local_gateway();
        // "app:s3cret"
        let request =
            format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nProxy-Authorization: Basic YXBwOnMzY3JldA==\r\n\r\n");
        let (result, answer) = exchange(&gateway, &apps(Some("s3cret")), request.as_bytes()).await;
        assert!(result.is_err());
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        let forward = format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n\r\n");
        assert!(exchange(&gateway, &apps(None), forward.as_bytes()).await.1.starts_with("HTTP/1.1 503 "));
        let mut socks = b"\x05\x01\x00\x05\x01\x00\x01\x7f\x00\x00\x01".to_vec();
        socks.extend(port.to_be_bytes());
        let (result, answer) = exchange(&gateway, &apps(None), &socks).await;
        assert!(result.is_err());
        assert_eq!(answer.as_bytes()[2..4], [5, GENERAL_FAILURE]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing reached the server");
    }

    #[tokio::test]
    async fn the_browser_listener_speaks_no_http() {
        let (result, answer) =
            exchange(&local_gateway(), &browser(), b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
        assert!(result.is_err());
        assert!(answer.is_empty(), "{answer}");
    }

    #[tokio::test]
    async fn refuses_heads_a_server_could_read_differently() {
        let gateway = local_gateway();
        for (request, status) in [
            (&b"GET http://a.example/ HTTP/1.1\r\nX: a\nInjected: b\r\n\r\n"[..], "400"),
            (b"GET http://a.example/ HTTP/1.1\r\nX: a\r\n folded\r\n\r\n", "400"),
            (b"GET https://a.example/ HTTP/1.1\r\n\r\n", "400"),
            (b"GET /relative HTTP/1.1\r\n\r\n", "400"),
            (b"CONNECT a.example HTTP/1.1\r\n\r\n", "400"),
            (b"GET http://a.example/ HTTP/2\r\n\r\n", "400"),
            (b"CONNECT a.i2p\rInjected:80 HTTP/1.1\r\n\r\n", "400"),
        ] {
            let (result, answer) = exchange(&gateway, &Access::Open, request).await;
            assert!(result.is_err(), "{}", String::from_utf8_lossy(request));
            assert!(answer.starts_with(&format!("HTTP/1.1 {status} ")), "{answer}");
        }
        let mut huge = b"GET http://a.example/ HTTP/1.1\r\nX: ".to_vec();
        huge.extend(vec![b'a'; MAX_HTTP_HEAD + 10]);
        let (result, answer) = exchange(&gateway, &Access::Open, &huge).await;
        assert!(result.is_err());
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");
    }

    #[tokio::test]
    async fn gives_up_on_a_client_that_says_nothing() {
        let (_client, server) = tokio::io::duplex(1024);
        let started = std::time::Instant::now();
        let error = proxy(server, &local_gateway(), &Access::Open, Duration::from_millis(50)).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn closes_clients_past_the_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let access = Arc::new(Access::Open);
        tokio::spawn(serve_with(
            listener,
            Arc::new(local_gateway()),
            access,
            CancellationToken::new(),
            1,
            HANDSHAKE_TIMEOUT,
        ));
        let _first = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut second = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut buf = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), second.read(&mut buf)).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "the second client is closed at once: {read:?}");
    }

    #[test]
    fn reads_basic_credentials_and_targets_strictly() {
        assert_eq!(base64_decode("dXNlcjpwYXNz").unwrap(), b"user:pass");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert_eq!(base64_decode("").unwrap(), b"");
        for bad in ["YQ=", "Y===", "YQ==YQ==", "@@@@", "YW\u{e9}="] {
            assert!(base64_decode(bad).is_none(), "{bad}");
        }
        assert_eq!(basic_credentials("basic dXNlcjpwYXNz"), Some(("user".into(), "pass".into())));
        assert_eq!(basic_credentials("Basic OnM="), Some((String::new(), "s".into())), "an empty user");
        assert!(basic_credentials("Bearer dXNlcjpwYXNz").is_none());
        assert_eq!(authority("[::1]:443"), Some(("::1".into(), 443)));
        assert_eq!(authority("x.onion:80"), Some(("x.onion".into(), 80)));
        for bad in ["x.onion", "x.onion:http", ":80", "user@x:80"] {
            assert!(authority(bad).is_none(), "{bad}");
        }
    }

    #[tokio::test]
    async fn refuses_host_names_a_router_could_misread() {
        let (target, written) = negotiate(b"\x05\x01\x00\x05\x01\x00\x03\x0aa.i2p\r\nX:1\x00\x50").await;
        assert!(target.is_err());
        assert_eq!(written[2..4], [5, NOT_ALLOWED]);
        assert!(valid_host("stats.i2p") && valid_host("_dmarc.example") && valid_host("::1") && valid_host("10.0.0.1"));
        assert!(!valid_host("") && !valid_host("a b") && !valid_host("a/b") && !valid_host(&"a".repeat(254)));
    }

    #[tokio::test]
    async fn checks_the_host_of_a_request_in_absolute_form_too() {
        // Found by fuzz/fuzz_targets/proxy_handshake.rs: `G http:) HTTP/1.1`
        // asked for the host `)`. The URL parser takes such names, and only
        // CONNECT's and SOCKS's were checked.
        for request in
            [&b"G http:) HTTP/1.1\r\n\r\n"[..], b"GET http://stats.)2p/a?b=1 HTTP/1.1\r\nHost: stats.i2p\r\n\r\n"]
        {
            let (target, written) = negotiate(request).await;
            assert!(target.is_err(), "{}", String::from_utf8_lossy(request));
            assert!(written.starts_with(b"HTTP/1.1 400 "), "{}", String::from_utf8_lossy(&written));
        }
        let (target, _) = negotiate(b"GET http://stats.i2p/a?b=1 HTTP/1.1\r\n\r\n").await;
        assert_eq!(target.unwrap(), ("stats.i2p".into(), 80));
    }

    /// An i2pd stand-in: answers every CONNECT with 200, then echoes.
    async fn echoing_router() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = stream.read_u8().await else { return };
                        head.push(byte);
                    }
                    stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                    let (mut read, mut write) = stream.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn new_identity_ends_the_browsers_open_streams() {
        let router = echoing_router().await;
        let config = GatewayConfig {
            tor_dir: None,
            i2p_proxies: vec![router.parse().unwrap()],
            fproxies: vec![],
            ..GatewayConfig::default()
        };
        let gateway = Arc::new(Gateway::new(config).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, gateway.clone(), Arc::new(browser()), CancellationToken::new()));
        let open = || async {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            client
                .write_all(b"\x05\x01\x02\x01\x09stats.i2p\x06s3cret\x05\x01\x00\x03\x09stats.i2p\x00\x50")
                .await
                .unwrap();
            let mut replies = [0u8; 14];
            client.read_exact(&mut replies).await.unwrap();
            assert_eq!(replies[4..6], [5, SUCCEEDED]);
            client.write_all(b"ping").await.unwrap();
            let mut echoed = [0u8; 4];
            client.read_exact(&mut echoed).await.unwrap();
            assert_eq!(&echoed, b"ping");
            client
        };
        let mut before = open().await;
        gateway.new_identity();
        let mut buf = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), before.read(&mut buf)).await;
        assert!(matches!(read, Ok(Ok(0))), "the stream from before ends: {read:?}");
        let _after = open().await;
    }
}

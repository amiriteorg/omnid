//! A SOCKS5 proxy (RFC 1928) in front of the gateway. Point a browser at it
//! with remote DNS ("socks5h") and `.onion` and `.i2p` addresses work like any
//! other.
//!
//! [`Access::Open`] serves `Gateway::connect`: the clearnet goes direct, or
//! through Tor with `clearnet_via_tor`. [`Access::Browser`] serves the app's
//! own browser through `Gateway::connect_browser`: only clients with the
//! password get in, everything goes through Tor (or i2pd), and the username,
//! the site the browser is on, keeps each site on circuits of its own.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use omnid_net::{FetchError, Gateway};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

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

/// Who may use a listener, and how its streams are routed.
pub enum Access {
    /// Anyone who can reach it; credentials are accepted and ignored.
    Open,
    /// Only clients that send this password (RFC 1929). The username names the
    /// site the browser is on.
    Browser { password: String },
}

/// A CONNECT request that passed the handshake.
struct Request {
    host: String,
    port: u16,
    username: Option<String>,
}

pub async fn serve(listener: TcpListener, gateway: Arc<Gateway>, access: Arc<Access>, shutdown: CancellationToken) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((client, _)) => {
                let (gateway, access) = (gateway.clone(), access.clone());
                tokio::spawn(async move {
                    if let Err(e) = proxy(client, &gateway, &access).await {
                        tracing::debug!("socks: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("socks: accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn proxy<S: AsyncRead + AsyncWrite + Unpin>(mut client: S, gateway: &Gateway, access: &Access) -> io::Result<()> {
    let Request { host, port, username } = handshake(&mut client, access).await?;
    let connected = match access {
        Access::Open => gateway.connect(&host, port).await,
        Access::Browser { .. } => gateway.connect_browser(&host, port, username.as_deref().unwrap_or_default()).await,
    };
    match connected {
        Ok(mut upstream) => {
            reply(&mut client, SUCCEEDED).await?;
            tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
            Ok(())
        }
        Err(e) => {
            reply(&mut client, reply_code(&e)).await?;
            Err(io::Error::other(format!("{host}:{port}: {e}")))
        }
    }
}

/// Negotiates a method, checks credentials if `access` needs them, and reads
/// a CONNECT request.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, access: &Access) -> io::Result<Request> {
    let [version, count] = read_array(s).await?;
    if version != VERSION {
        return Err(invalid("not a SOCKS5 client"));
    }
    let mut methods = vec![0u8; count.into()];
    s.read_exact(&mut methods).await?;
    // An open listener takes credentials and ignores them, so clients that
    // always send them (to isolate their streams in Tor) still work.
    let acceptable: &[u8] = match access {
        Access::Open => &[NO_AUTH, USER_PASS],
        Access::Browser { .. } => &[USER_PASS],
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
        if let Access::Browser { password } = access
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
        reply(s, COMMAND_NOT_SUPPORTED).await?;
        return Err(invalid("only CONNECT is supported"));
    }
    let host = match address_type {
        0x01 => Ipv4Addr::from(read_array::<4, _>(s).await?).to_string(),
        0x03 => {
            let [len] = read_array(s).await?;
            let mut name = vec![0u8; len.into()];
            s.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| invalid("host name is not UTF-8"))?
        }
        0x04 => Ipv6Addr::from(read_array::<16, _>(s).await?).to_string(),
        _ => {
            reply(s, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(invalid("unknown address type"));
        }
    };
    let port = u16::from_be_bytes(read_array(s).await?);
    Ok(Request { host, port, username })
}

/// The bound address is not meaningful here, so it is reported as 0.0.0.0:0.
async fn reply<S: AsyncWrite + Unpin>(s: &mut S, code: u8) -> io::Result<()> {
    s.write_all(&[VERSION, code, 0, 0x01, 0, 0, 0, 0, 0, 0]).await
}

fn reply_code(error: &FetchError) -> u8 {
    match error {
        FetchError::PrivateAddress(_) => NOT_ALLOWED,
        FetchError::Timeout(_) => TTL_EXPIRED,
        FetchError::Unavailable { .. } => GENERAL_FAILURE,
        _ => HOST_UNREACHABLE,
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
        assert!(proxy(server, &gateway, &Access::Open).await.is_err());
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
        assert!(proxy(server, &gateway, &browser()).await.is_err());
        let mut replies = [0u8; 14];
        client.read_exact(&mut replies).await.unwrap();
        assert_eq!(replies[4..6], [5, GENERAL_FAILURE], "Tor is off, so nothing is reachable");
        let accepted = tokio::time::timeout(std::time::Duration::from_millis(200), target.accept()).await;
        assert!(accepted.is_err(), "nothing connected to the server");
    }
}

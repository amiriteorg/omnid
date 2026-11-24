//! Yggdrasil: an IPv6 network of its own, 200::/7, reached through a node on
//! this machine (whose tunnel interface gives it an address in the range) or
//! through yggstack's SOCKS5 proxy, which needs neither root nor a tunnel.
//! It does not hide who connects: a site sees the node's address.

use std::net::{IpAddr, Ipv6Addr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

use crate::BoxError;

/// The Yggdrasil address of a node on this machine, if one runs here.
#[cfg(unix)]
pub(crate) fn local_node() -> Option<Ipv6Addr> {
    crate::tailnet::interfaces().into_iter().find_map(|(_, ip)| match ip {
        IpAddr::V6(ip) if omnid_core::is_yggdrasil(ip) => Some(ip),
        _ => None,
    })
}

/// Elsewhere only yggstack is used.
#[cfg(not(unix))]
pub(crate) fn local_node() -> Option<Ipv6Addr> {
    None
}

/// A stream to `ip`, port `port`, through the SOCKS5 proxy at `proxy`
/// (`socks5://host:port`, yggstack's `-socks`), which takes no password.
pub(crate) async fn socks5(proxy: &Url, ip: Ipv6Addr, port: u16) -> Result<TcpStream, BoxError> {
    let host = proxy.host_str().ok_or("a SOCKS proxy without a host")?.trim_matches(['[', ']']);
    let mut stream = TcpStream::connect((host, proxy.port().unwrap_or(1080))).await?;
    stream.write_all(&[5, 1, 0]).await?;
    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await?;
    match choice {
        [5, 0] => {}
        [5, _] => return Err("the SOCKS proxy wants a password; yggstack's does not".into()),
        _ => return Err("not a SOCKS5 proxy".into()),
    }
    let mut request = vec![5, 1, 0, 4];
    request.extend_from_slice(&ip.octets());
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err("not a SOCKS5 proxy".into());
    }
    if head[1] != 0 {
        return Err(format!("the SOCKS proxy could not connect: {}", reply_text(head[1])).into());
    }
    // The address the proxy bound, which is of no use here.
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(stream.read_u8().await?),
        _ => return Err("a SOCKS5 reply with an unknown address type".into()),
    };
    let mut bound = vec![0u8; skip + 2];
    stream.read_exact(&mut bound).await?;
    Ok(stream)
}

/// RFC 1928's reply codes, in words.
fn reply_text(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "not allowed by its rules",
        3 => "network unreachable",
        4 => "host unreachable (is the node connected to peers?)",
        5 => "connection refused",
        6 => "timed out",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "an unknown error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SOCKS5 proxy on 127.0.0.1 that answers the greeting with `choice`
    /// and a CONNECT with `reply`, then echoes; returns its URL and what it
    /// was asked to connect to.
    async fn proxy(choice: [u8; 2], reply: &'static [u8]) -> (Url, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("socks5://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&choice).await.unwrap();
            let mut request = vec![0u8; 22];
            if stream.read_exact(&mut request).await.is_err() {
                return Vec::new();
            }
            stream.write_all(reply).await.unwrap();
            let mut echo = [0u8; 5];
            if stream.read_exact(&mut echo).await.is_ok() {
                stream.write_all(&echo).await.unwrap();
            }
            request
        });
        (url, task)
    }

    #[tokio::test]
    async fn connects_through_a_socks5_proxy() {
        let (url, asked) = proxy([5, 0], &[5, 0, 0, 1, 127, 0, 0, 1, 0, 80]).await;
        let ip: Ipv6Addr = "200:b48d:469e:c7c7:3e13:c41d:ba4d:d2b8".parse().unwrap();
        let mut stream = socks5(&url, ip, 80).await.unwrap();
        stream.write_all(b"hello").await.unwrap();
        let mut echo = [0u8; 5];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"hello");
        let request = asked.await.unwrap();
        assert_eq!(request[..4], [5, 1, 0, 4], "CONNECT to an IPv6 address");
        assert_eq!(request[4..20], ip.octets());
        assert_eq!(request[20..], 80u16.to_be_bytes());
    }

    #[tokio::test]
    async fn says_why_a_proxy_would_not_connect() {
        let ip: Ipv6Addr = "200::1".parse().unwrap();
        let (url, _) = proxy([5, 0], &[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        assert!(socks5(&url, ip, 80).await.unwrap_err().to_string().contains("peers"));
        let (url, _) = proxy([5, 2], b"").await;
        assert!(socks5(&url, ip, 80).await.unwrap_err().to_string().contains("password"));
        let (url, _) = proxy([4, 0], b"").await;
        assert!(socks5(&url, ip, 80).await.unwrap_err().to_string().contains("not a SOCKS5"));
    }
}

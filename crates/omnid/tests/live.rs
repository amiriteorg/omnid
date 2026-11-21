//! The whole daemon over the real Tor network. Ignored by default; run with
//! `cargo test -p omnid -- --ignored` or `scripts/live-routers.sh test`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use omnid::DaemonConfig;
use omnid_crawl::{CrawlConfig, Safety};
use omnid_net::GatewayConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOR_PROJECT: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: Tor network"]
async fn live_socks_proxy_opens_onions_through_the_daemon() {
    let data_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("daemon-socks");
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let daemon = omnid::start(DaemonConfig {
        gateway: GatewayConfig { tor_dir: Some(data_dir.join("tor")), ..GatewayConfig::default() },
        data_dir,
        listen: loopback,
        crawl: CrawlConfig::default(),
        safety: Safety::default(),
        seeds: vec![],
        api_token: Some("live".into()),
        start_paused: true,
        socks: Some(loopback),
        browser_proxy: None,
        dht: None,
        dht_harvest: false,
    })
    .await
    .unwrap();

    let mut socks = tokio::net::TcpStream::connect(daemon.socks_addr().unwrap()).await.unwrap();
    let mut request = vec![5, 1, 0, 5, 1, 0, 3, TOR_PROJECT.len() as u8];
    request.extend_from_slice(TOR_PROJECT.as_bytes());
    request.extend_from_slice(&80u16.to_be_bytes());
    socks.write_all(&request).await.unwrap();
    let mut replies = [0u8; 12];
    socks.read_exact(&mut replies).await.unwrap();
    assert_eq!(replies[..4], [5, 0, 5, 0], "method chosen, CONNECT succeeded");

    let get = format!("GET / HTTP/1.1\r\nHost: {TOR_PROJECT}\r\nConnection: close\r\n\r\n");
    socks.write_all(get.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socks.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200"), "{}", &response[..80.min(response.len())]);
    assert!(response.contains("Tor Project"));

    daemon.shutdown();
    daemon.wait().await.unwrap();
}

/// A GET through a SOCKS5 proxy with a username and password; the raw
/// response, or the proxy's refusal as `Err(reply code)`.
async fn get_via(proxy: SocketAddr, user: &str, password: &str, host: &str) -> Result<String, u8> {
    let mut socks = tokio::net::TcpStream::connect(proxy).await.unwrap();
    let mut greeting = vec![5, 1, 2, 1, user.len() as u8];
    greeting.extend_from_slice(user.as_bytes());
    greeting.push(password.len() as u8);
    greeting.extend_from_slice(password.as_bytes());
    socks.write_all(&greeting).await.unwrap();
    // The answer before the request, as browsers do: the proxy hangs up on a
    // wrong password, and bytes it never read would turn that into a reset.
    let mut auth = [0u8; 4];
    socks.read_exact(&mut auth).await.unwrap();
    if auth[3] != 0 {
        return Err(auth[3]);
    }
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&80u16.to_be_bytes());
    socks.write_all(&request).await.unwrap();
    let mut reply = [0u8; 10];
    socks.read_exact(&mut reply).await.unwrap();
    if reply[1] != 0 {
        return Err(reply[1]);
    }
    let get = format!("GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: curl/8\r\nConnection: close\r\n\r\n");
    socks.write_all(get.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socks.read_to_end(&mut response).await.unwrap();
    Ok(String::from_utf8_lossy(&response).into_owned())
}

/// The body of an HTTP response.
fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").map_or("", |(_, body)| body).trim()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: Tor network and icanhazip.com"]
async fn live_browser_proxy_goes_only_through_tor() {
    let data_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("daemon-browser");
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let daemon = omnid::start(DaemonConfig {
        gateway: GatewayConfig { tor_dir: Some(data_dir.join("tor")), ..GatewayConfig::default() },
        data_dir,
        listen: loopback,
        crawl: CrawlConfig::default(),
        safety: Safety::default(),
        seeds: vec![],
        api_token: Some("live".into()),
        start_paused: true,
        socks: None,
        browser_proxy: Some("live-secret".into()),
        dht: None,
        dht_harvest: false,
    })
    .await
    .unwrap();
    let proxy = daemon.browser_proxy_addr().unwrap();

    let onion = get_via(proxy, TOR_PROJECT, "live-secret", TOR_PROJECT).await.unwrap();
    assert!(onion.starts_with("HTTP/1.1 200") && onion.contains("Tor Project"), "{}", &onion[..80.min(onion.len())]);
    assert_eq!(get_via(proxy, TOR_PROJECT, "wrong", TOR_PROJECT).await.err(), Some(1), "a wrong password is refused");

    // The clearnet leaves from a Tor exit, not from this machine.
    let via_tor = get_via(proxy, "icanhazip.com", "live-secret", "icanhazip.com").await.unwrap();
    let mut direct = tokio::net::TcpStream::connect(("icanhazip.com", 80)).await.unwrap();
    direct
        .write_all(b"GET / HTTP/1.1\r\nHost: icanhazip.com\r\nUser-Agent: curl/8\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    direct.read_to_end(&mut response).await.unwrap();
    let direct = String::from_utf8_lossy(&response).into_owned();
    let (tor_ip, own_ip) = (body(&via_tor), body(&direct));
    assert!(!tor_ip.is_empty() && !own_ip.is_empty(), "{via_tor}\n{direct}");
    assert_ne!(tor_ip, own_ip, "the browser's clearnet went direct");

    daemon.new_identity();
    daemon.shutdown();
    daemon.wait().await.unwrap();
}

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

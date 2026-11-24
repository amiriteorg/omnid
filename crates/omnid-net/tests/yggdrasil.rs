//! Yggdrasil through a stand-in for yggstack's SOCKS5 proxy on 127.0.0.1,
//! which also plays the site: pages fetched through it, its state in the
//! status, and the browser's route never taking it.

use std::sync::{Arc, Mutex};

use omnid_core::{Network, Source};
use omnid_net::{FetchError, FetchOptions, Gateway, GatewayConfig, RouterState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const SITE: &str = "http://[200:b48d:469e:c7c7:3e13:c41d:ba4d:d2b8]/";

/// A SOCKS5 proxy that takes any CONNECT and answers the HTTP request that
/// follows with a page; returns its URL and the addresses it was asked for.
async fn yggstack() -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("socks5://{}", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut greeting = [0u8; 3];
                if stream.read_exact(&mut greeting).await.is_err() {
                    return;
                }
                stream.write_all(&[5, 0]).await.unwrap();
                let mut request = [0u8; 22];
                stream.read_exact(&mut request).await.unwrap();
                let ip = std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&request[4..20]).unwrap());
                log.lock().unwrap().push(format!("[{ip}]:{}", u16::from_be_bytes([request[20], request[21]])));
                stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(stream.read_u8().await.unwrap());
                }
                let body = "<title>Yggdrasil search</title><a href=\"/about\">about</a>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    (url, asked)
}

fn gateway(socks: Vec<Url>, yggdrasil: bool) -> Gateway {
    Gateway::new(GatewayConfig { tor_dir: None, yggdrasil, yggdrasil_socks: socks, ..GatewayConfig::default() })
        .unwrap()
}

#[tokio::test]
async fn fetches_a_site_through_yggstack() {
    let (proxy, asked) = yggstack().await;
    let gateway = gateway(vec![proxy.clone()], true);
    let page = gateway.fetch(&Source::parse(SITE).unwrap(), &FetchOptions::default()).await.unwrap();
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(page.body.as_ref().unwrap()).contains("Yggdrasil search"));
    assert_eq!(asked.lock().unwrap().as_slice(), ["[200:b48d:469e:c7c7:3e13:c41d:ba4d:d2b8]:80"]);
    let status = gateway.status().await.into_iter().find(|r| r.network == Network::Yggdrasil).unwrap();
    assert_eq!(status.state, RouterState::Ready);
    assert_eq!(status.detail, format!("yggstack at {proxy}"));
}

#[tokio::test]
async fn without_a_way_in_it_defers_and_says_why() {
    let gateway = gateway(vec![], true);
    let missing = gateway.fetch(&Source::parse(SITE).unwrap(), &FetchOptions::default()).await;
    assert!(matches!(missing, Err(FetchError::Unavailable { network: Network::Yggdrasil, .. })), "{missing:?}");
    let off = self::gateway(vec![], false);
    let status = off.status().await.into_iter().find(|r| r.network == Network::Yggdrasil).unwrap();
    assert_eq!(status.state, RouterState::Disabled);
}

#[tokio::test]
async fn the_browser_never_takes_it() {
    let (proxy, asked) = yggstack().await;
    let gateway = gateway(vec![proxy], true);
    let refused = gateway.connect_browser("[200:b48d:469e:c7c7:3e13:c41d:ba4d:d2b8]", 80, "site").await;
    assert!(refused.is_err());
    assert!(asked.lock().unwrap().is_empty(), "not through yggstack either");
}

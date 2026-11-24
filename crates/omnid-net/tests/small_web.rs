//! Gopher through the gateway, against a stand-in hole on 127.0.0.1: menus
//! and text items fetched, error items as "not found", binaries never asked
//! for, and the built-in browser's route never direct. Gemini's exchange is
//! tested in `src/gemini.rs`, TLS aside; `live.rs` asks real capsules.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omnid_core::Source;
use omnid_net::{FetchError, FetchOptions, GOPHER_MENU_TYPE, Gateway, GatewayConfig, Route, SmallWeb};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A Gopher server that answers each selector from `items` (an error item
/// for others), counting connections.
async fn hole(items: &'static [(&'static str, &'static str)]) -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = connections.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n") {
                    let Ok(byte) = stream.read_u8().await else { return };
                    request.push(byte);
                }
                let selector = String::from_utf8_lossy(&request[..request.len() - 2]).into_owned();
                let answer = items
                    .iter()
                    .find(|(s, _)| *s == selector)
                    .map(|(_, a)| a.to_string())
                    .unwrap_or_else(|| format!("3'{selector}' does not exist\terror.host\t1\r\n.\r\n"));
                let _ = stream.write_all(answer.as_bytes()).await;
            });
        }
    });
    (port, connections)
}

const ITEMS: &[(&str, &str)] = &[
    ("", "iWelcome\t\terror.host\t1\r\n1Phlog\t/phlog/\t127.0.0.1\t70\r\n.\r\n"),
    ("/about.txt", "About this hole.\r\n"),
];

fn gateway(allow_private_addresses: bool) -> Gateway {
    Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        ipfs_gateways: vec![],
        allow_private_addresses,
        ..GatewayConfig::default()
    })
    .unwrap()
}

fn source(port: u16, path: &str) -> Source {
    Source::parse(&format!("gopher://127.0.0.1:{port}{path}")).unwrap()
}

#[tokio::test]
async fn fetches_menus_and_text_items() {
    let (port, _) = hole(ITEMS).await;
    let gateway = gateway(true);
    let menu = gateway.fetch(&source(port, "/"), &FetchOptions::default()).await.unwrap();
    assert_eq!((menu.status, menu.content_type.as_deref()), (200, Some(GOPHER_MENU_TYPE)));
    assert!(menu.body.unwrap().starts_with(b"iWelcome"));
    let text = gateway.fetch(&source(port, "/0/about.txt"), &FetchOptions::default()).await.unwrap();
    assert_eq!(
        (text.content_type.as_deref(), text.body.as_deref()),
        (Some("text/plain"), Some(&b"About this hole.\r\n"[..]))
    );
}

#[tokio::test]
async fn an_error_item_is_not_found() {
    let (port, _) = hole(ITEMS).await;
    let gateway = gateway(true);
    let page = gateway.fetch(&source(port, "/0/missing.txt"), &FetchOptions::default()).await.unwrap();
    assert_eq!((page.status, page.body), (404, None));
    let answer = gateway.small_web(&source(port, "/1/gone/"), &FetchOptions::default(), &Route::Direct).await;
    assert_eq!(answer.unwrap(), SmallWeb::Failed { status: 51, message: "'/gone/' does not exist".into() });
}

#[tokio::test]
async fn binaries_are_never_asked_for() {
    let (port, connections) = hole(ITEMS).await;
    let gateway = gateway(true);
    for path in ["/9/file.zip", "/I/photo.jpg", "/7/search"] {
        assert!(gateway.fetch(&source(port, path), &FetchOptions::default()).await.is_err(), "{path}");
    }
    assert_eq!(connections.load(Ordering::SeqCst), 0, "not even a connection");
}

#[tokio::test]
async fn a_type_the_caller_refuses_is_not_asked_for() {
    let (port, connections) = hole(ITEMS).await;
    let opts = FetchOptions { accept: |ct| ct == Some("text/plain"), ..FetchOptions::default() };
    let menu = gateway(true).fetch(&source(port, "/"), &opts).await.unwrap();
    assert_eq!((menu.status, menu.body), (200, None));
    assert_eq!(connections.load(Ordering::SeqCst), 0, "not even a connection");
}

#[tokio::test]
async fn private_addresses_are_refused_unless_allowed() {
    let (port, connections) = hole(ITEMS).await;
    let refused = gateway(false).fetch(&source(port, "/"), &FetchOptions::default()).await;
    assert!(matches!(refused, Err(FetchError::PrivateAddress(_))), "{refused:?}");
    assert_eq!(connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_browser_s_route_is_never_direct() {
    let (port, connections) = hole(ITEMS).await;
    let route = Route::Browser { site: "127.0.0.1".into() };
    let answer = gateway(true).small_web(&source(port, "/"), &FetchOptions::default(), &route).await;
    assert!(answer.is_err(), "without Tor there is no way there");
    assert_eq!(connections.load(Ordering::SeqCst), 0);
}

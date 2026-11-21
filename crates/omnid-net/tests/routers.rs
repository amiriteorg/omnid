//! Several routers for one network: a request goes to the next when one is
//! down or cannot reach the site, against local stand-ins for i2pd's HTTP
//! proxy and for Hyphanet nodes.

use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use omnid_core::Source;
use omnid_net::{FetchOptions, Gateway, GatewayConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

/// An HTTP proxy that answers every request itself, as i2pd answers a GET for
/// an eepsite, with `status` and `body`.
async fn router(status: StatusCode, body: &'static str) -> Url {
    let app = axum::Router::new().fallback(get(move || async move { (status, Html(body)) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/").parse().unwrap()
}

/// A port nothing listens on.
fn down() -> Url {
    "http://127.0.0.1:9/".parse().unwrap()
}

fn gateway(i2p_proxies: Vec<Url>, fproxies: Vec<Url>) -> Gateway {
    Gateway::new(GatewayConfig { tor_dir: None, i2p_proxies, fproxies, ..GatewayConfig::default() }).unwrap()
}

fn opts() -> FetchOptions {
    FetchOptions { accept: |_| true, ..FetchOptions::default() }
}

async fn fetch(gateway: &Gateway, address: &str) -> (u16, String) {
    let page = gateway.fetch(&Source::parse(address).unwrap(), &opts()).await.unwrap();
    (page.status, String::from_utf8_lossy(page.body.as_deref().unwrap_or_default()).into_owned())
}

#[tokio::test]
async fn fetches_through_the_next_router_when_one_is_down() {
    let up = router(StatusCode::OK, "<title>stats</title>").await;
    let gateway = gateway(vec![down(), up], vec![]);
    assert_eq!(fetch(&gateway, "http://stats.i2p/").await, (200, "<title>stats</title>".into()));
    // The one that was down now rests: the next request goes straight on.
    let status = gateway.status().await;
    let i2p = status.iter().find(|s| s.network == omnid_core::Network::I2p).unwrap();
    assert_eq!(i2p.state, omnid_net::RouterState::Ready, "{}", i2p.detail);
    assert!(i2p.detail.starts_with("no i2pd HTTP proxy at http://127.0.0.1:9/; i2pd HTTP proxy at"), "{}", i2p.detail);
}

#[tokio::test]
async fn asks_another_router_when_one_cannot_reach_the_site() {
    let lost = router(StatusCode::INTERNAL_SERVER_ERROR, "i2pd: Host is down").await;
    let found = router(StatusCode::OK, "<title>reg</title>").await;
    let both = gateway(vec![lost.clone(), found], vec![]);
    assert_eq!(fetch(&both, "http://reg.i2p/").await.0, 200);
    let alone = gateway(vec![lost], vec![]);
    assert_eq!(fetch(&alone, "http://reg.i2p/").await, (500, "i2pd: Host is down".into()), "the last answer stands");
}

#[tokio::test]
async fn hyphanet_keys_go_to_the_next_node() {
    let node = router(StatusCode::OK, "<title>a freesite</title>").await;
    let gateway = gateway(vec![], vec![down(), node.clone()]);
    let key = "freenet:USK@AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA,BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB,AQACAAE/site/1/";
    assert_eq!(fetch(&gateway, key).await.1, "<title>a freesite</title>");
    assert_eq!(gateway.fproxy(), Some(&node), "links go to the node that answers");
}

/// Echoes every connection back to itself.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn streams_go_to_the_next_router_and_node() {
    let echo = echo_server().await;
    let node: Url = format!("http://127.0.0.1:{echo}/").parse().unwrap();
    let gateway = gateway(vec![down()], vec![down(), node]);
    let mut stream = gateway.connect_browser("hyphanet", 80, "hyphanet").await.unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"ping");
    let error = gateway.connect_browser("stats.i2p", 80, "stats.i2p").await.err().expect("no router is up");
    assert!(error.to_string().contains("is the router running"), "{error}");
}

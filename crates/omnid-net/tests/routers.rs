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

/// Counts the connections a clearnet stand-in accepts, answering each with a
/// page.
async fn watched_clearnet_site() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().fallback(get(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async { Html("<title>seen you</title>") }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, hits)
}

/// An i2pd stand-in whose eepsites all redirect to `location`, as an HTTP
/// proxy (absolute-form GETs) and through CONNECT tunnels alike.
async fn redirecting_router(location: String) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let location = location.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = stream.read_u8().await else { return };
                    head.push(byte);
                }
                if head.starts_with(b"CONNECT ") {
                    stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                    head.clear();
                    while !head.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = stream.read_u8().await else { return };
                        head.push(byte);
                    }
                }
                let answer = format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n");
                let _ = stream.write_all(answer.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/").parse().unwrap()
}

#[tokio::test]
async fn a_redirect_off_an_anonymity_network_is_not_followed_directly() {
    // The clearnet is direct and may reach loopback: following the redirect
    // would show the site's author this machine's address.
    let (port, hits) = watched_clearnet_site().await;
    let watched = format!("http://127.0.0.1:{port}/who");
    let router = redirecting_router(watched.clone()).await;
    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        allow_private_addresses: true,
        i2p_proxies: vec![router],
        fproxies: vec![],
        ..GatewayConfig::default()
    })
    .unwrap();
    assert!(gateway.leaves_anonymity(omnid_core::Network::I2p, omnid_core::Network::Clearnet));
    assert!(!gateway.leaves_anonymity(omnid_core::Network::Clearnet, omnid_core::Network::Clearnet));
    let page = gateway.fetch(&Source::parse("http://trap.i2p/").unwrap(), &opts()).await.unwrap();
    assert_eq!(page.status, 302, "the redirect comes back unfollowed");
    assert_eq!(page.redirect.map(|r| r.to_string()), Some(watched.clone()));

    let file = tempfile_for_test();
    let error = gateway
        .download(
            &"http://trap.i2p/file".parse().unwrap(),
            &file,
            &omnid_net::DownloadOptions::default(),
            &omnid_net::Progress::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("from an anonymity network to the clearnet"), "{error}");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing reached the watched site");

    // Through Tor exits, the clearnet hides the address: the redirect is fine.
    let via_tor =
        Gateway::new(GatewayConfig { tor_dir: None, clearnet_via_tor: true, ..GatewayConfig::default() }).unwrap();
    assert!(!via_tor.leaves_anonymity(omnid_core::Network::Onion, omnid_core::Network::Clearnet));
}

fn tempfile_for_test() -> std::fs::File {
    let path = std::env::temp_dir().join(format!("omnid-redirect-test-{}", std::process::id()));
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap()
}

#[tokio::test]
async fn the_browsers_downloads_send_what_the_browser_sends() {
    // An i2pd stand-in that tunnels to a file server and keeps the request
    // the download sent through it.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let kept = seen.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router: Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        for _ in 0..2 {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            if head.starts_with(b"CONNECT ") {
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            } else {
                *kept.lock().unwrap() = String::from_utf8(head).unwrap();
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nfile").await.unwrap();
            }
        }
    });
    let gateway = gateway(vec![router], vec![]);
    let options = omnid_net::DownloadOptions {
        route: omnid_net::Route::Browser { site: "site.i2p".into() },
        connections: 1,
        ..omnid_net::DownloadOptions::default()
    };
    let file = tempfile_for_test();
    let done = gateway
        .download(
            &"http://site.i2p/file".parse().unwrap(),
            &file,
            &options,
            &omnid_net::Progress::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(done.bytes, 4);
    let head = seen.lock().unwrap().to_ascii_lowercase();
    assert!(head.contains("accept-language: en-us,en;q=0.5\r\n"), "{head}");
    assert!(head.contains("accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8\r\n"), "{head}");
    assert!(head.contains("sec-gpc: 1\r\n"), "{head}");
}

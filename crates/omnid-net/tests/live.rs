//! Live tests against the real Tor network, i2pd and a Hyphanet node.
//! Ignored by default; `scripts/live-routers.sh all` starts the routers and
//! runs them. A test whose router is not running fails and says how to start
//! it, rather than passing.

use std::path::PathBuf;
use std::time::Duration;

use omnid_core::Source;
use omnid_net::{FetchOptions, Gateway, GatewayConfig, Page};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOR_PROJECT: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const BBC: &str = "www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion";
/// Hyphanet's own introduction, which every node bookmarks.
const FIRST_STEPS: &str = "USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps";
/// I2P's standard anonymized User-Agent; some eepsites refuse any other.
const I2P_USER_AGENT: &str = "MYOB/6.66 (AN/ON)";

/// Tor state persists between runs in Cargo's temp directory, so only the
/// first run downloads the consensus. One directory per test: arti locks
/// its state, and tests run in parallel.
fn gateway(test: &str, cfg: GatewayConfig) -> Gateway {
    let tor_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tor").join(test);
    Gateway::new(GatewayConfig { tor_dir: Some(tor_dir), ..cfg }).unwrap()
}

fn require_router(port: u16, name: &str) {
    if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        panic!("{name} is not running on 127.0.0.1:{port}; start it with scripts/live-routers.sh up");
    }
}

async fn fetch(gateway: &Gateway, address: &str) -> Page {
    let source = Source::parse(address).unwrap();
    gateway.fetch(&source, &FetchOptions::default()).await.unwrap_or_else(|e| panic!("{address}: {e}"))
}

/// Fetches, retrying twice: overlay sites and fresh routers are flaky.
async fn fetch_with_retries(gateway: &Gateway, address: &str) -> Page {
    let source = Source::parse(address).unwrap();
    let mut last = String::new();
    for _ in 0..3 {
        match gateway.fetch(&source, &FetchOptions::default()).await {
            Ok(page) if page.status == 200 => return page,
            Ok(page) => last = format!("HTTP {}", page.status),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    panic!("{address}: {last}");
}

fn text(page: &Page) -> String {
    String::from_utf8_lossy(page.body.as_deref().unwrap_or_default()).into_owned()
}

/// Sends a GET over a raw stream and returns the status line.
async fn status_line(stream: &mut omnid_net::BoxStream, host: &str, user_agent: &str) -> String {
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {user_agent}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    // arti buffers writes until flushed.
    stream.flush().await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n") && head.len() < 256 && stream.read(&mut byte).await.unwrap() == 1 {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).trim_end().to_owned()
}

#[tokio::test]
#[ignore = "live: Tor network"]
async fn live_tor_fetches_http_and_https_onions() {
    let gateway = gateway("fetches", GatewayConfig::default());
    let tor = fetch(&gateway, &format!("http://{TOR_PROJECT}/")).await;
    assert_eq!(tor.status, 200);
    assert!(text(&tor).contains("<title>Tor Project"), "{}", &text(&tor)[..200.min(text(&tor).len())]);

    let bbc = fetch(&gateway, &format!("https://{BBC}/")).await;
    assert_eq!(bbc.status, 200, "TLS over Tor, verified against the web PKI");
    assert!(text(&bbc).contains("BBC"));
}

#[tokio::test]
#[ignore = "live: Tor network"]
async fn live_tor_reuses_connections_across_pages() {
    let gateway = gateway("reuse", GatewayConfig::default());
    for path in ["/", "/download/", "/about/history/", "/"] {
        let page = fetch(&gateway, &format!("http://{TOR_PROJECT}{path}")).await;
        assert_eq!(page.status, 200, "{path}");
    }
}

#[tokio::test]
#[ignore = "live: Tor network and check.torproject.org"]
async fn live_clearnet_via_tor_exits_through_tor() {
    let check = "https://check.torproject.org/api/ip";
    let via_tor = gateway("exit", GatewayConfig { clearnet_via_tor: true, ..GatewayConfig::default() });
    // Exit relays reset connections now and then; retried.
    let via_tor = fetch_with_retries(&via_tor, check).await;
    assert!(text(&via_tor).contains("\"IsTor\":true"), "{}", text(&via_tor));
    let direct =
        fetch(&Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap(), check).await;
    assert!(text(&direct).contains("\"IsTor\":false"), "{}", text(&direct));
}

#[tokio::test]
#[ignore = "live: Tor network"]
async fn live_tor_opens_raw_streams_to_onions() {
    let gateway = gateway("streams", GatewayConfig::default());
    let mut stream = gateway.connect(TOR_PROJECT, 80).await.unwrap();
    let status = status_line(&mut stream, TOR_PROJECT, "omnid-test").await;
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
}

#[tokio::test]
#[ignore = "live: needs i2pd (scripts/live-routers.sh up)"]
async fn live_i2p_fetches_eepsites_through_i2pd() {
    require_router(4444, "i2pd");
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    let page = fetch_with_retries(&gateway, "http://stats.i2p/").await;
    assert!(text(&page).contains("stats.i2p"), "{}", text(&page));
}

#[tokio::test]
#[ignore = "live: needs i2pd (scripts/live-routers.sh up)"]
async fn live_i2p_tunnels_raw_streams_through_i2pd() {
    require_router(4444, "i2pd");
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    let mut last = String::new();
    for _ in 0..3 {
        match gateway.connect("reg.i2p", 80).await {
            Ok(mut stream) => {
                last = status_line(&mut stream, "reg.i2p", I2P_USER_AGENT).await;
                if last.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    panic!("reg.i2p through a CONNECT tunnel: {last}");
}

#[tokio::test]
#[ignore = "live: needs i2pd (scripts/live-routers.sh up)"]
async fn live_browser_reaches_eepsites_through_i2pd() {
    // The browser's route: .i2p through i2pd, with Tor off to show it needs
    // nothing else.
    require_router(4444, "i2pd");
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    let mut last = String::new();
    for _ in 0..3 {
        match gateway.connect_browser("reg.i2p", 80, "reg.i2p").await {
            Ok(mut stream) => {
                last = status_line(&mut stream, "reg.i2p", I2P_USER_AGENT).await;
                if last.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    panic!("reg.i2p through the browser's route: {last}");
}

#[tokio::test]
#[ignore = "live: needs a Hyphanet node (scripts/live-routers.sh up)"]
async fn live_hyphanet_follows_newer_editions() {
    require_router(8888, "Hyphanet's FProxy");
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    // Edition 20 is old: FProxy redirects to the newest it knows.
    let page = fetch_with_retries(&gateway, &format!("freenet:{FIRST_STEPS}/20/")).await;
    let edition: u32 = page.source.to_string().split('/').nth(2).and_then(|e| e.parse().ok()).unwrap();
    assert!(edition >= 24, "{}", page.source);
    assert!(text(&page).contains("Freenet First Steps"));
}

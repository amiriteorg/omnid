//! Live tests against the real Tor network, i2pd, a Hyphanet node, an IPFS
//! node, Nostr relays, and Gemini and Gopher servers.
//! Ignored by default; `scripts/live-routers.sh all` starts the routers and
//! runs them. A test whose router is not running fails and says how to start
//! it, rather than passing.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use omnid_core::Source;
use omnid_net::{DownloadOptions, FetchOptions, Gateway, GatewayConfig, Page, Progress};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;

const TOR_PROJECT: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const BBC: &str = "www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion";
/// Hyphanet's own introduction, which every node bookmarks.
const FIRST_STEPS: &str = "USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps";
/// A few MB that do not change (Python 2.2's source, 2001), with ranges.
const FEW_MB: &str = "https://www.python.org/ftp/python/2.2/Python-2.2.tgz";
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
#[ignore = "live: Tor network and www.python.org"]
async fn live_tor_downloads_in_parallel_what_one_connection_does() {
    let gateway = gateway("download", GatewayConfig { clearnet_via_tor: true, ..GatewayConfig::default() });
    let url = Url::parse(FEW_MB).unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("live-download");
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    for connections in [1, 4] {
        let path = dir.join(connections.to_string());
        let opts = DownloadOptions { connections, min_segment: 256 << 10, ..DownloadOptions::default() };
        let mut last = String::new();
        let mut downloaded = None;
        // Exit relays reset connections now and then; retried.
        for _ in 0..3 {
            let file = File::create(&path).unwrap();
            match gateway.download(&url, &file, &opts, &Progress::default(), &CancellationToken::new()).await {
                Ok(download) => {
                    downloaded = Some(download);
                    break;
                }
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let download = downloaded.unwrap_or_else(|| panic!("{connections} connections: {last}"));
        assert!(download.ranged, "{connections} connections: python.org sends ranges");
        assert!(download.connections >= connections.min(2), "{connections} connections: {download:?}");
        assert_eq!(download.filename, "Python-2.2.tgz");
        files.push(std::fs::read(&path).unwrap());
    }
    assert!(files[0].len() > 2 << 20, "{} bytes", files[0].len());
    assert!(files[0] == files[1], "four connections wrote what one did, byte for byte");
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
#[ignore = "live: needs i2pd with SAM on (scripts/live-routers.sh up)"]
async fn live_browser_reaches_eepsites_through_sam() {
    // The browser's route through SAM alone, with no HTTP proxy to fall back
    // on: each site from a destination of its own, on 3-hop tunnels.
    require_router(omnid_net::DEFAULT_SAM_PORT, "i2pd's SAM bridge");
    let sam = format!("sam://127.0.0.1:{}", omnid_net::DEFAULT_SAM_PORT).parse().unwrap();
    let cfg = GatewayConfig { tor_dir: None, i2p_proxies: vec![], i2p_sam: vec![sam], ..GatewayConfig::default() };
    let gateway = Gateway::new(cfg).unwrap();
    for site in ["reg.i2p", "stats.i2p"] {
        let mut last = String::new();
        for _ in 0..3 {
            match gateway.connect_browser(site, 80, site).await {
                Ok(mut stream) => last = status_line(&mut stream, site, I2P_USER_AGENT).await,
                Err(e) => last = e.to_string(),
            }
            if last.starts_with("HTTP/1.1 ") {
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        assert!(last.starts_with("HTTP/1.1 "), "{site} through SAM: {last}");
    }
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

#[tokio::test]
#[ignore = "live: needs an IPFS node (scripts/live-routers.sh up)"]
async fn live_ipfs_fetches_cids_and_dnslink_sites() {
    require_router(8080, "An IPFS node's gateway");
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    // The empty directory, which every kubo repo holds, asked for by its
    // CIDv0: the gateway gets the CIDv1 and lists the directory.
    let page = fetch(&gateway, "ipfs://QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn/").await;
    assert_eq!(page.status, 200);
    assert_eq!(page.source.to_string(), "ipfs://bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354/");
    // A DNSLink site, over the IPFS network: the directory's redirect to its
    // slash stays in IPFS.
    let page = fetch_with_retries(&gateway, "ipns://docs.ipfs.tech/concepts").await;
    assert_eq!(page.source.to_string(), "ipns://docs.ipfs.tech/concepts/");
    assert!(text(&page).contains("IPFS Docs"), "{}", text(&page).chars().take(200).collect::<String>());
}

#[tokio::test]
#[ignore = "live: Nostr relays"]
async fn live_nostr_reads_a_profile_from_relays() {
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    // fiatjaf, who made Nostr.
    let fiatjaf: [u8; 32] = (0..32)
        .map(|i| {
            u8::from_str_radix(
                &"3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"[2 * i..2 * i + 2],
                16,
            )
            .unwrap()
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let filter =
        omnid_net::NostrFilter { authors: vec![fiatjaf], kinds: vec![0], limit: Some(1), ..Default::default() };
    let events = gateway.nostr(&filter, &[]).await.unwrap();
    assert!(!events.is_empty(), "no default relay had the profile");
    // Every event came back checked; the newest names him.
    let newest = events.iter().max_by_key(|e| e.created_at).unwrap();
    assert!(newest.content.contains("fiatjaf"), "{}", newest.content);
}

#[tokio::test]
#[ignore = "live: Geminispace and Gopherspace"]
async fn live_small_web_reads_a_capsule_and_a_hole() {
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    // Project Gemini's capsule, on a self-signed certificate.
    let capsule = fetch_with_retries(&gateway, "gemini://geminiprotocol.net/").await;
    assert!(capsule.content_type.as_deref().unwrap_or_default().starts_with("text/gemini"));
    assert!(text(&capsule).contains("Gemini"), "{}", text(&capsule).chars().take(200).collect::<String>());
    let hole = fetch_with_retries(&gateway, "gopher://gopher.floodgap.com/").await;
    assert_eq!(hole.content_type.as_deref(), Some(omnid_net::GOPHER_MENU_TYPE));
    assert!(text(&hole).contains("Floodgap"));
}

#[tokio::test]
#[ignore = "live: Tor network and an onion capsule"]
async fn live_tor_reads_an_onion_capsule() {
    let gateway = gateway("capsule", GatewayConfig::default());
    let capsule =
        fetch_with_retries(&gateway, "gemini://slowsvr7rgnkzuoma5byw34n2mkkg6v7g5q6qxqaihh7zikp5wn26fid.onion/").await;
    assert!(text(&capsule).contains("Slow"), "{}", text(&capsule).chars().take(200).collect::<String>());
}

//! Crawls a local site end to end: robots.txt, redirects, content-type
//! refusal, the blocked-terms gate, magnet harvesting and cross-network links.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, Crawler, Job, Politeness, SEED_TRUST, Safety};
use omnid_index::{SearchIndex, SearchQuery};
use omnid_net::{Gateway, GatewayConfig};
use tokio_util::sync::CancellationToken;

const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const MAGNET: &str = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=Ubuntu+ISO";

#[derive(Default)]
struct Visits {
    private: AtomicBool,
    behind_blocked: AtomicBool,
}

async fn serve() -> (String, Arc<Visits>) {
    let visits = Arc::new(Visits::default());
    let home = format!(
        r#"<title>Home directory</title>
        <a href="/a">a</a> <a href="/private/secret">private</a> <a href="/img.png">img</a>
        <a href="/blocked">blocked</a> <a href="/moved">moved</a>
        <a href="http://other-site.example/">elsewhere</a>
        <a href="{MAGNET}">torrent</a> <a href="http://{ONION}/">onion</a>"#
    );
    let (v1, v2) = (visits.clone(), visits.clone());
    let app = Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow: /private\n" }))
        .route("/", get(move || async move { Html(home) }))
        .route("/a", get(|| async { Html(r#"<title>Page A</title><p>all about apples</p><a href="/b">b</a>"#) }))
        .route("/b", get(|| async { Html("<title>Page B</title><p>all about bananas</p>") }))
        .route("/c", get(|| async { Html("<title>Page C</title><p>all about cherries</p>") }))
        .route("/moved", get(|| async { (StatusCode::MOVED_PERMANENTLY, [(LOCATION, "/c")]) }))
        .route("/img.png", get(|| async { ([(CONTENT_TYPE, "image/png")], vec![0x89u8, b'P', b'N', b'G']) }))
        .route(
            "/blocked",
            get(|| async { Html(r#"<p>this has the forbidden phrase</p><a href="/behind-blocked">x</a>"#) }),
        )
        .route(
            "/behind-blocked",
            get(move || async move {
                v1.behind_blocked.store(true, Ordering::SeqCst);
                Html("<p>should never be reached</p>").into_response()
            }),
        )
        .route(
            "/private/secret",
            get(move || async move {
                v2.private.store(true, Ordering::SeqCst);
                Html("<p>top secret</p>").into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, visits)
}

fn hits(index: &SearchIndex, text: &str, networks: &[Network]) -> Vec<omnid_index::SearchHit> {
    index.search(&SearchQuery { text: text.into(), networks: networks.to_vec(), limit: 10 }).unwrap()
}

#[tokio::test]
async fn crawls_a_site_end_to_end() {
    let (base, visits) = serve().await;

    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxy: None,
        fproxy: None,
        allow_private_addresses: true,
        ..GatewayConfig::default()
    })
    .unwrap();
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let safety = Safety::from_lists("", "forbidden phrase");
    let no_delay = Politeness { concurrency: 4, site_delay: Duration::ZERO };
    let cfg = CrawlConfig {
        politeness: HashMap::from([(Network::Clearnet, no_delay)]),
        commit_interval: Duration::from_millis(100),
        ..CrawlConfig::default()
    };
    let crawler = Crawler::new(Arc::new(gateway), index.clone(), Arc::new(safety), cfg);

    let (jobs, rx) = tokio::sync::mpsc::channel(16);
    jobs.send(Job { source: Source::parse(&base).unwrap(), depth: 0, trust: SEED_TRUST }).await.unwrap();
    drop(jobs);
    let stats = tokio::time::timeout(Duration::from_secs(30), crawler.run(rx, CancellationToken::new()))
        .await
        .expect("crawl finishes once the frontier drains");

    // Home, a, b and c (through the redirect), plus the magnet.
    assert_eq!(stats.indexed, 5, "{stats:?}");
    for word in ["directory", "apples", "bananas", "cherries"] {
        assert_eq!(hits(&index, word, &[]).len(), 1, "{word}");
    }
    let seed = &hits(&index, "directory", &[])[0];
    assert_eq!(seed.routes[0].trust, SEED_TRUST);

    // robots.txt kept the crawler out of /private.
    assert!(!visits.private.load(Ordering::SeqCst));
    assert!(hits(&index, "secret", &[]).is_empty());

    // The blocked page is not indexed and its links were not followed.
    assert_eq!(stats.blocked, 1, "{stats:?}");
    assert!(hits(&index, "forbidden", &[]).is_empty());
    assert!(!visits.behind_blocked.load(Ordering::SeqCst));

    // The image was refused by content type, so it was never indexed.
    assert!(stats.skipped >= 2, "image and redirect: {stats:?}");

    // The magnet link was harvested into the index without a fetch.
    let magnets = hits(&index, "ubuntu", &[Network::Magnet]);
    assert_eq!(magnets.len(), 1);
    assert_eq!(magnets[0].info_hash.as_deref(), Some("c12fe1c06bba254a9dc9f519b335aa7c1367a88a"));

    // The onion link was followed and failed because Tor is off; the
    // off-site clearnet link was not followed at all.
    assert_eq!(stats.failed, 1, "{stats:?}");
}

#[tokio::test]
async fn refuses_private_addresses_by_default() {
    let (base, _) = serve().await;
    let gateway = Gateway::new(GatewayConfig { tor_dir: None, ..GatewayConfig::default() }).unwrap();
    let error = gateway.fetch(&Source::parse(&base).unwrap(), &Default::default()).await.unwrap_err();
    assert!(matches!(error, omnid_net::FetchError::PrivateAddress(_)), "{error}");

    let localhost = base.replace("127.0.0.1", "localhost");
    let error = gateway.fetch(&Source::parse(&localhost).unwrap(), &Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("non-public"), "{error}");
}

#[tokio::test]
async fn reports_missing_routers_plainly() {
    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxy: Some("http://127.0.0.1:9".parse().unwrap()),
        ..GatewayConfig::default()
    })
    .unwrap();
    let error = gateway.fetch(&Source::parse("http://stats.i2p/").unwrap(), &Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("is the router running"), "{error}");
    let error =
        gateway.fetch(&Source::parse(&format!("http://{ONION}/")).unwrap(), &Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("Tor is disabled"), "{error}");
}

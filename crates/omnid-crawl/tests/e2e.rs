//! Crawls a local site end to end: robots.txt, redirects, content-type
//! refusal, the blocked-terms gate, magnet harvesting, cross-network links,
//! resuming from the crawl store, and pausing.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, CrawlStore, Crawler, Job, Politeness, SEED_TRUST, Safety};
use omnid_index::{SearchIndex, SearchQuery};
use omnid_net::{Gateway, GatewayConfig};
use tokio_util::sync::CancellationToken;

const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
const MAGNET: &str = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=Ubuntu+ISO";

#[derive(Default)]
struct Visits {
    home: AtomicUsize,
    private: AtomicBool,
    behind_blocked: AtomicBool,
}

async fn serve() -> (String, Arc<Visits>) {
    let visits = Arc::new(Visits::default());
    let home = format!(
        r#"<title>Home directory</title>
        <a href="/a">a</a> <a href="/private/secret">private</a> <a href="/img.png">img</a>
        <a href="/blocked">blocked</a> <a href="/moved">moved</a> <a href="/away">away</a>
        <a href="http://other-site.example/">elsewhere</a>
        <a href="{MAGNET}">torrent</a> <a href="http://{ONION}/">onion</a>"#
    );
    let (v0, v1, v2) = (visits.clone(), visits.clone(), visits.clone());
    let app = Router::new()
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow: /private\n" }))
        .route(
            "/",
            get(move || async move {
                v0.home.fetch_add(1, Ordering::SeqCst);
                Html(home)
            }),
        )
        .route("/a", get(|| async { Html(r#"<title>Page A</title><p>all about apples</p><a href="/b">b</a>"#) }))
        .route("/b", get(|| async { Html("<title>Page B</title><p>all about bananas</p>") }))
        .route("/c", get(|| async { Html("<title>Page C</title><p>all about cherries</p>") }))
        .route("/moved", get(|| async { (StatusCode::MOVED_PERMANENTLY, [(LOCATION, "/c")]) }))
        .route("/away", get(|| async { (StatusCode::FOUND, [(LOCATION, "http://elsewhere.example/")]) }))
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
    index.search(&SearchQuery { text: text.into(), networks: networks.to_vec(), offset: 0, limit: 10 }).unwrap().hits
}

fn crawler(index: Arc<SearchIndex>, store: Arc<CrawlStore>, max_pages: Option<u64>) -> Crawler {
    crawler_with(index, store, CrawlConfig { max_pages, ..CrawlConfig::default() })
}

fn crawler_with(index: Arc<SearchIndex>, store: Arc<CrawlStore>, cfg: CrawlConfig) -> Crawler {
    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxy: None,
        fproxy: None,
        allow_private_addresses: true,
        ..GatewayConfig::default()
    })
    .unwrap();
    let safety = Safety::from_lists("", "forbidden phrase");
    let no_delay = Politeness { concurrency: 4, site_delay: Duration::ZERO };
    let cfg = CrawlConfig {
        politeness: HashMap::from([(Network::Clearnet, no_delay)]),
        commit_interval: Duration::from_millis(100),
        ..cfg
    };
    Crawler::new(Arc::new(gateway), index, Arc::new(safety), store, cfg)
}

/// Runs a crawl to completion, seeded with `seeds`.
async fn crawl(crawler: Crawler, seeds: &[&str]) -> omnid_crawl::StatsSnapshot {
    let (jobs, rx) = tokio::sync::mpsc::channel(16);
    for seed in seeds {
        jobs.send(Job { source: Source::parse(seed).unwrap(), depth: 0, trust: SEED_TRUST }).await.unwrap();
    }
    drop(jobs);
    tokio::time::timeout(Duration::from_secs(30), crawler.run(rx, CancellationToken::new()))
        .await
        .expect("crawl finishes once nothing is due")
}

#[tokio::test]
async fn crawls_a_site_end_to_end() {
    let (base, visits) = serve().await;
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let stats = crawl(crawler(index.clone(), store, None), &[&base]).await;

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
    assert!(stats.skipped >= 3, "image and two redirects: {stats:?}");

    // The magnet link was harvested into the index without a fetch.
    let magnets = hits(&index, "ubuntu", &[Network::Magnet]);
    assert_eq!(magnets.len(), 1);
    assert_eq!(magnets[0].info_hash.as_deref(), Some("c12fe1c06bba254a9dc9f519b335aa7c1367a88a"));

    // The onion link was followed and failed because Tor is off; the
    // off-site clearnet link and the off-site redirect were not followed.
    assert_eq!(stats.failed, 1, "{stats:?}");
}

#[tokio::test]
async fn resumes_from_the_store_without_refetching_fresh_pages() {
    let (base, visits) = serve().await;
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("crawl.sqlite");
    let index = Arc::new(SearchIndex::open(&dir.path().join("index")).unwrap());

    // Stopped after two jobs: home and one of its links.
    let store = Arc::new(CrawlStore::open(&store_path).unwrap());
    let first = crawl(crawler(index.clone(), store, Some(2)), &[&base]).await;
    assert_eq!(first.indexed + first.skipped + first.failed, 2, "{first:?}");
    drop(index);

    // A new process with no seeds picks up the rest from the store.
    let index = Arc::new(SearchIndex::open(&dir.path().join("index")).unwrap());
    let store = Arc::new(CrawlStore::open(&store_path).unwrap());
    let second = crawl(crawler(index.clone(), store, None), &[]).await;
    assert_eq!(first.indexed + second.indexed, 5, "{second:?}");
    assert_eq!(visits.home.load(Ordering::SeqCst), 1, "the fresh home page was not fetched again");
    for word in ["directory", "apples", "bananas", "cherries"] {
        assert_eq!(hits(&index, word, &[]).len(), 1, "{word}");
    }
}

#[tokio::test]
async fn drains_a_store_bigger_than_the_working_set_without_stalling() {
    let (base, _) = serve().await;
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    // One job in memory at a time: every page after the first comes from a
    // refill. The crawl loop used to nap 5 s whenever the working set ran dry.
    let cfg = CrawlConfig { frontier_capacity: 1, ..CrawlConfig::default() };
    let crawler = crawler_with(index, Arc::new(CrawlStore::in_memory().unwrap()), cfg);
    let started = std::time::Instant::now();
    let stats = crawl(crawler, &[&base]).await;
    assert_eq!(stats.indexed, 5, "{stats:?}");
    assert!(started.elapsed() < Duration::from_secs(4), "took {:?}", started.elapsed());
}

#[tokio::test]
async fn pausing_stops_new_fetches() {
    let (base, visits) = serve().await;
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let crawler = crawler(index, Arc::new(CrawlStore::in_memory().unwrap()), None);
    let control = crawler.control();
    control.set_paused(true);
    let run = tokio::spawn(async move { crawl(crawler, &[&base]).await });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(visits.home.load(Ordering::SeqCst), 0);
    assert!(control.stats().paused);
    control.set_paused(false);
    let stats = run.await.unwrap();
    assert_eq!(stats.indexed, 5, "{stats:?}");
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

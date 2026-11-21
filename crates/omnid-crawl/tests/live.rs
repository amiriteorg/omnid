//! Live end-to-end crawls over real networks. Ignored by default;
//! `scripts/live-routers.sh all` starts i2pd and Hyphanet and runs them. A
//! test whose router is not running fails and says how to start it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, CrawlStore, Crawler, Job, Politeness, SEED_TRUST, Safety, StatsSnapshot};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SearchHit, SearchIndex, SearchQuery};
use omnid_net::{Gateway, GatewayConfig};
use tokio_util::sync::CancellationToken;

const TOR_PROJECT: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/";
const SINTEL: &str = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10";
const FIRST_STEPS: &str = "freenet:USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/";

fn require_router(port: u16, name: &str) {
    if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        panic!("{name} is not running on 127.0.0.1:{port}; start it with scripts/live-routers.sh up");
    }
}

/// Tor state per test, kept between runs; arti locks its directory.
fn tor_dir(test: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tor").join(test)
}

/// Crawls from `seed` until `max_pages` jobs ran or nothing is due, with a
/// short politeness delay so the test is quick but still one page at a time.
async fn crawl(gateway: GatewayConfig, dht: Option<Dht>, seed: &str, max_pages: u64) -> (StatsSnapshot, SearchIndex) {
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let brief = Politeness { concurrency: 4, site_delay: Duration::from_millis(500) };
    let cfg = CrawlConfig {
        max_depth: 1,
        max_pages: Some(max_pages),
        commit_interval: Duration::from_secs(1),
        politeness: Network::ALL.iter().map(|n| (*n, brief)).collect::<HashMap<_, _>>(),
        ..CrawlConfig::default()
    };
    let mut crawler = Crawler::new(
        Arc::new(Gateway::new(gateway).unwrap()),
        index.clone(),
        Arc::new(Safety::default()),
        Arc::new(CrawlStore::in_memory().unwrap()),
        cfg,
    );
    if let Some(dht) = dht {
        crawler = crawler.with_dht(dht);
    }
    let (jobs, rx) = tokio::sync::mpsc::channel(1);
    jobs.send(Job { source: Source::parse(seed).unwrap(), depth: 0, trust: SEED_TRUST }).await.unwrap();
    drop(jobs);
    let stats = tokio::time::timeout(Duration::from_secs(600), crawler.run(rx, CancellationToken::new()))
        .await
        .expect("the crawl finishes");
    let index = Arc::try_unwrap(index).ok().expect("the crawler is done with the index");
    (stats, index)
}

fn hits(index: &SearchIndex, text: &str, network: Network) -> Vec<SearchHit> {
    index.search(&SearchQuery { text: text.into(), networks: vec![network], offset: 0, limit: 20 }).unwrap().hits
}

#[tokio::test]
#[ignore = "live: Tor network"]
async fn live_crawls_an_onion_site_over_tor() {
    let gateway = GatewayConfig { tor_dir: Some(tor_dir("crawl")), ..GatewayConfig::default() };
    let (stats, index) = crawl(gateway, None, TOR_PROJECT, 6).await;
    assert!(stats.indexed >= 4, "{stats:?}");
    let found = hits(&index, "tor browser", Network::Onion);
    assert!(!found.is_empty(), "{stats:?}");
    assert!(found.iter().all(|h| h.routes.iter().all(|r| r.url.contains(".onion/"))));
}

#[tokio::test]
#[ignore = "live: BitTorrent DHT"]
async fn live_indexes_a_magnet_from_dht_metadata() {
    let dht = Dht::start(DhtConfig::default()).await.unwrap();
    let gateway = GatewayConfig { tor_dir: None, ..GatewayConfig::default() };
    let (stats, index) = crawl(gateway, Some(dht), SINTEL, 1).await;
    assert_eq!(stats.indexed, 1, "{stats:?}");
    let found = hits(&index, "sintel", Network::Magnet);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].files.len(), 11, "{:?}", found[0].files);
    assert!(found[0].description.starts_with("11 files"), "{}", found[0].description);
    assert!(found[0].routes[0].liveness > 0.0, "liveness comes from the swarm");
}

#[tokio::test]
#[ignore = "live: needs i2pd (scripts/live-routers.sh up)"]
async fn live_crawls_an_eepsite_through_i2pd() {
    require_router(4444, "i2pd");
    let gateway = GatewayConfig { tor_dir: None, ..GatewayConfig::default() };
    let (stats, index) = crawl(gateway, None, "http://reg.i2p/", 4).await;
    assert!(stats.indexed >= 1, "{stats:?}");
    assert!(!hits(&index, "", Network::I2p).is_empty());
}

#[tokio::test]
#[ignore = "live: needs a Hyphanet node (scripts/live-routers.sh up)"]
async fn live_crawls_freesites_through_fproxy() {
    require_router(8888, "Hyphanet's FProxy");
    let gateway = GatewayConfig { tor_dir: None, ..GatewayConfig::default() };
    let (stats, index) = crawl(gateway, None, FIRST_STEPS, 6).await;
    assert!(stats.indexed >= 3, "{stats:?}");
    assert_eq!(stats.failed, 0, "{stats:?}");
    let found = hits(&index, "freenet", Network::Hyphanet);
    assert!(
        found.iter().any(|h| h.title == "Freenet First Steps"),
        "{:?}",
        found.iter().map(|h| &h.title).collect::<Vec<_>>()
    );
}

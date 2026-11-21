//! Crawl throughput against a local synthetic site: the crawler's own cost
//! per page (fetch over loopback, extraction, store, index), with network
//! latency taken out. One site, so pages are fetched one at a time, as the
//! politeness rules require.
//!
//!     cargo run --release -p omnid-crawl --example bench_crawl -- [pages] [rounds]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Path;
use axum::response::Html;
use axum::routing::get;
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, CrawlStore, Crawler, Job, Politeness, SEED_TRUST, Safety};
use omnid_index::SearchIndex;
use omnid_net::{Gateway, GatewayConfig};
use tokio_util::sync::CancellationToken;

const WORDS: [&str; 32] = [
    "onion",
    "service",
    "mirror",
    "index",
    "archive",
    "library",
    "forum",
    "market",
    "privacy",
    "relay",
    "circuit",
    "network",
    "search",
    "crawler",
    "garlic",
    "tunnel",
    "freesite",
    "torrent",
    "magnet",
    "peer",
    "swarm",
    "hash",
    "router",
    "gateway",
    "directory",
    "wiki",
    "blog",
    "news",
    "guide",
    "manual",
    "release",
    "download",
];
const LINKS_PER_PAGE: usize = 10;
const WORDS_PER_PAGE: usize = 1_000;

/// Deterministic page `i` of a site of `pages`: a title, ~8 KB of text and
/// links to ten other pages.
fn page(i: usize, pages: usize) -> String {
    let mut state = (i as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut text = String::with_capacity(WORDS_PER_PAGE * 8);
    for _ in 0..WORDS_PER_PAGE {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        text.push_str(WORDS[(state >> 59) as usize]);
        text.push(' ');
    }
    let links: String =
        (1..=LINKS_PER_PAGE).map(|k| format!(r#"<a href="/p/{}">next {k}</a> "#, (i * 7 + k * 13) % pages)).collect();
    format!("<!doctype html><title>Page {i}</title><meta name=description content='page {i}'><p>{text}</p>{links}")
}

async fn serve(pages: usize) -> String {
    let app = Router::new().route("/p/{i}", get(move |Path(i): Path<usize>| async move { Html(page(i, pages)) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn round(base: &str) -> (u64, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let gateway =
        Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
            .unwrap();
    let index = Arc::new(SearchIndex::open(&dir.path().join("index")).unwrap());
    let store = Arc::new(CrawlStore::open(&dir.path().join("crawl.sqlite")).unwrap());
    let cfg = CrawlConfig {
        max_depth: 1_000,
        politeness: HashMap::from([(Network::Clearnet, Politeness { concurrency: 64, site_delay: Duration::ZERO })]),
        ..CrawlConfig::default()
    };
    let crawler = Crawler::new(Arc::new(gateway), index, Arc::new(Safety::default()), store, cfg);
    let (jobs, rx) = tokio::sync::mpsc::channel(1);
    jobs.send(Job { source: Source::parse(&format!("{base}p/0")).unwrap(), depth: 0, trust: SEED_TRUST })
        .await
        .unwrap();
    drop(jobs);
    let started = Instant::now();
    let stats = crawler.run(rx, CancellationToken::new()).await;
    (stats.indexed, started.elapsed())
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let pages: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(2_000);
    let rounds: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let base = serve(pages).await;

    let mut rates = Vec::new();
    for r in 1..=rounds {
        let (indexed, elapsed) = round(&base).await;
        let rate = indexed as f64 / elapsed.as_secs_f64();
        println!("round {r}: {indexed} pages in {:.2} s = {rate:.0} pages/s", elapsed.as_secs_f64());
        rates.push(rate);
    }
    rates.sort_by(f64::total_cmp);
    let median = rates[rates.len() / 2];
    println!(
        "pages/s over {rounds} rounds: min {:.0}  median {median:.0}  max {:.0}",
        rates[0],
        rates[rates.len() - 1]
    );
}

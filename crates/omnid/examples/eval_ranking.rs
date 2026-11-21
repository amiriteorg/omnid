//! Ranking evaluation on a real multilingual site: the Tor Project's onion,
//! which serves the same pages in dozens of languages.
//!
//!     cargo run --release -p omnid --example eval_ranking -- fetch target/eval/torproject 400
//!     cargo run --release -p omnid --example eval_ranking -- run target/eval/torproject
//!
//! `fetch` crawls the site once over Tor (robots.txt honoured, one request a
//! second) and caches every page. `run` serves the cache from a local server,
//! crawls it with the real crawler into a fresh index, and scores judged
//! queries, so every version of the code ranks identical input. The judged
//! pages are English: the queries are what an English speaker would type.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use omnid_core::{Network, Source};
use omnid_crawl::extract::extract;
use omnid_crawl::{CrawlConfig, CrawlStore, Crawler, Job, Politeness, SEED_TRUST, Safety, is_indexable_type};
use omnid_index::{SearchIndex, SearchQuery};
use omnid_net::{FetchOptions, Gateway, GatewayConfig};
use tokio_util::sync::CancellationToken;

const SITE: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

/// Query, then the English pages that answer it (paths on the site), written
/// from those pages' titles and headings before any run. The onion's home
/// page is cached twice, as `/` and `/?noredirect=1`.
const JUDGMENTS: &[(&str, &[&str])] = &[
    ("tor project", HOME),
    ("anonymity online", HOME),
    ("tor browser", HOME),
    ("download tor browser", HOME),
    ("browse privately", HOME),
    ("circumvent censorship", HOME),
    ("tor history", &["/about/history/"]),
    ("history", &["/about/history/"]),
    ("jobs", &["/about/jobs/"]),
    ("contact", &["/contact/"]),
    ("press", &["/press/"]),
    ("privchat", &["/privchat/"]),
    ("people", &["/about/people/"]),
    ("supporters", &["/about/supporters/"]),
    ("reports", &["/about/reports/"]),
    ("trademark", &["/about/trademark/"]),
    ("privacy policy", &["/about/privacy_policy/"]),
    ("board of directors", &["/about/jobs/board-of-directors/"]),
    ("ux lead", &["/about/jobs/ux-lead/"]),
];

const HOME: &[&str] = &["/", "/?noredirect=1"];

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fetch") => fetch(Path::new(&args[1]), args.get(2).and_then(|n| n.parse().ok()).unwrap_or(400)).await,
        Some("run") => run(Path::new(&args[1])).await,
        _ => eprintln!("usage: eval_ranking fetch <dir> [pages] | run <dir>"),
    }
}

/// Breadth-first over the site, two links deep, caching each HTML page as
/// `NNNN.html` with its path and content type in `manifest.tsv`.
async fn fetch(dir: &Path, max_pages: usize) {
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    let gateway = Gateway::new(GatewayConfig { tor_dir: Some(dir.join("tor")), ..GatewayConfig::default() }).unwrap();
    let opts = FetchOptions { accept: is_indexable_type, max_redirects: 0, ..FetchOptions::default() };
    let robots = gateway.fetch(&Source::parse(&format!("{SITE}/robots.txt")).unwrap(), &FetchOptions::default()).await;
    let robots =
        robots.ok().and_then(|page| page.body).and_then(|body| texting_robots::Robot::new("omnid", &body).ok());

    let mut manifest = String::new();
    let mut seen = HashSet::from(["/".to_owned()]);
    let mut queue = VecDeque::from([("/".to_owned(), 0)]);
    let mut saved = 0;
    while let Some((path, depth)) = queue.pop_front() {
        if saved >= max_pages {
            break;
        }
        if robots.as_ref().is_some_and(|r| !r.allowed(&path)) {
            continue;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let source = Source::parse(&format!("{SITE}{path}")).unwrap();
        let page = match gateway.fetch(&source, &opts).await {
            Ok(page) => page,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        let (Some(body), 200) = (page.body, page.status) else {
            continue;
        };
        let file = format!("{saved:04}.html");
        std::fs::write(dir.join("pages").join(&file), &body).unwrap();
        let content_type = page.content_type.unwrap_or_default();
        manifest.push_str(&format!("{path}\t{content_type}\t{file}\n"));
        saved += 1;
        if depth < 2 {
            for href in extract(Some(&content_type), &body).links {
                // The onion mirrors www.torproject.org (its Onion-Location),
                // and its navigation links point at the clearnet site.
                let url = match source.join(&href) {
                    Some(Source::Onion(url)) if url.host_str() == source.url().unwrap().host_str() => url,
                    Some(Source::Clearnet(url)) if url.host_str() == Some("www.torproject.org") => url,
                    _ => continue,
                };
                let path = url[url::Position::BeforePath..url::Position::AfterQuery].to_owned();
                if seen.insert(path.clone()) {
                    queue.push_back((path, depth + 1));
                }
            }
        }
        if saved % 25 == 0 {
            eprintln!("{saved} pages");
        }
    }
    std::fs::write(dir.join("manifest.tsv"), manifest).unwrap();
    eprintln!("cached {saved} pages in {}", dir.display());
}

#[derive(Clone)]
struct Cache(Arc<HashMap<String, (String, PathBuf)>>);

async fn serve_cached(State(cache): State<Cache>, request: Request) -> Response {
    let path = request.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    match cache.0.get(path) {
        Some((content_type, file)) => {
            ([(header::CONTENT_TYPE, content_type.clone())], std::fs::read(file).unwrap()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn run(dir: &Path) {
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).expect("run fetch first");
    let pages: HashMap<String, (String, PathBuf)> = manifest
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (path, content_type, file) = (fields.next()?, fields.next()?, fields.next()?);
            Some((path.to_owned(), (content_type.to_owned(), dir.join("pages").join(file))))
        })
        .collect();
    let app = Router::new().fallback(serve_cached).with_state(Cache(Arc::new(pages.clone())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Every cached page is a seed, so the index holds exactly the cache.
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let gateway =
        Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
            .unwrap();
    let cfg = CrawlConfig {
        max_depth: 0,
        politeness: HashMap::from([(Network::Clearnet, Politeness { concurrency: 8, site_delay: Duration::ZERO })]),
        ..CrawlConfig::default()
    };
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let crawler = Crawler::new(Arc::new(gateway), index.clone(), Arc::new(Safety::default()), store, cfg);
    let (jobs, rx) = tokio::sync::mpsc::channel(pages.len().max(1));
    for path in pages.keys() {
        let source = Source::parse(&format!("{base}{path}")).unwrap();
        jobs.try_send(Job { source, depth: 0, trust: SEED_TRUST }).unwrap();
    }
    drop(jobs);
    let stats = crawler.run(rx, CancellationToken::new()).await;
    println!("indexed {} of {} cached pages", stats.indexed, pages.len());

    // `judgments.tsv` in the cache (query, then relevant paths separated by
    // spaces) replaces the built-in list, so one binary can score new
    // judgments.
    let judgments: Vec<(String, Vec<String>)> = match std::fs::read_to_string(dir.join("judgments.tsv")) {
        Ok(file) => file
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .map(|(q, paths)| (q.to_owned(), paths.split_whitespace().map(str::to_owned).collect()))
            .collect(),
        Err(_) => {
            JUDGMENTS.iter().map(|(q, p)| ((*q).to_owned(), p.iter().map(|p| (*p).to_owned()).collect())).collect()
        }
    };
    let languages: Vec<String> =
        std::env::var("EVAL_LANGUAGES").map(|l| l.split(',').map(str::to_owned).collect()).unwrap_or_default();
    let mut reciprocal_ranks = Vec::new();
    let mut foreign_in_top3 = 0;
    println!("{:<22} {:>5}  top result", "query", "rank");
    for (text, relevant) in &judgments {
        let query = eval_query(text, &languages);
        let hits = index.search(&query).unwrap().hits;
        let path_of = |hit: &omnid_index::SearchHit| {
            hit.routes.iter().map(|r| r.url.trim_start_matches(&base).to_owned()).collect::<Vec<_>>()
        };
        let rank = hits.iter().position(|hit| path_of(hit).iter().any(|p| relevant.contains(p))).map(|i| i + 1);
        reciprocal_ranks.push(rank.map_or(0.0, |r| 1.0 / r as f64));
        foreign_in_top3 += hits.iter().take(3).filter(|hit| path_of(hit).iter().all(|p| is_translation(p))).count();
        let top = hits.first().map(|h| path_of(h).join(" ")).unwrap_or_default();
        println!("{text:<22} {:>5}  {top}", rank.map_or("-".into(), |r| r.to_string()));
    }
    let n = judgments.len() as f64;
    let at_one = reciprocal_ranks.iter().filter(|r| **r == 1.0).count();
    println!(
        "success@1 {at_one}/{}  MRR@10 {:.3}  translations in top 3: {foreign_in_top3}/{}",
        judgments.len(),
        reciprocal_ranks.iter().sum::<f64>() / n,
        judgments.len() * 3
    );
}

/// The query an English reader sends, with their browser's languages
/// (`EVAL_LANGUAGES=en`); none when unset.
fn eval_query(text: &str, languages: &[String]) -> SearchQuery {
    SearchQuery { text: text.into(), limit: 10, languages: languages.to_vec(), ..SearchQuery::default() }
}

/// The site serves translations under `/<language>/`.
fn is_translation(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or_default();
    (2..=5).contains(&first.len())
        && first.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
        && !matches!(first, "about" | "blog" | "faq" | "help" | "news" | "press")
}

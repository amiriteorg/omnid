//! Crawls a local site end to end: robots.txt, redirects, content-type
//! refusal, the blocked-terms gate, magnet harvesting, cross-network links,
//! resuming from the crawl store, and pausing; and a local Gopher hole.

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
    image: AtomicBool,
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
    let (v0, v1, v2, v3) = (visits.clone(), visits.clone(), visits.clone(), visits.clone());
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
        .route(
            "/img.png",
            get(move || async move {
                v3.image.store(true, Ordering::SeqCst);
                ([(CONTENT_TYPE, "image/png")], vec![0x89u8, b'P', b'N', b'G'])
            }),
        )
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
    index
        .search(&SearchQuery { text: text.into(), networks: networks.to_vec(), limit: 10, ..SearchQuery::default() })
        .unwrap()
        .hits
}

fn crawler(index: Arc<SearchIndex>, store: Arc<CrawlStore>, max_pages: Option<u64>) -> Crawler {
    crawler_with(index, store, CrawlConfig { max_pages, ..CrawlConfig::default() })
}

fn crawler_with(index: Arc<SearchIndex>, store: Arc<CrawlStore>, cfg: CrawlConfig) -> Crawler {
    crawler_with_safety(index, store, cfg, Safety::from_lists("", "forbidden phrase"))
}

fn crawler_with_safety(index: Arc<SearchIndex>, store: Arc<CrawlStore>, cfg: CrawlConfig, safety: Safety) -> Crawler {
    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        allow_private_addresses: true,
        ..GatewayConfig::default()
    })
    .unwrap();
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
        jobs.send(Job { source: Source::parse(seed).unwrap(), depth: 0, site_depth: 0, trust: SEED_TRUST })
            .await
            .unwrap();
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

    // The image was not even fetched: its name says it is one.
    assert!(!visits.image.load(Ordering::SeqCst));
    assert!(stats.skipped >= 2, "two redirects: {stats:?}");

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
async fn revisits_drop_vanished_and_newly_blocked_pages() {
    let gone = Arc::new(AtomicBool::new(false));
    let flag = gone.clone();
    let app = Router::new()
        .route("/robots.txt", get(|| async { StatusCode::NOT_FOUND }))
        .route("/fruit", get(|| async { Html("<title>Fruit</title><p>all about apples</p>") }))
        .route(
            "/vanishing",
            get(move || {
                let gone = flag.load(Ordering::SeqCst);
                async move {
                    if gone {
                        StatusCode::NOT_FOUND.into_response()
                    } else {
                        Html("<p>a vanishing page</p>").into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let seeds = [format!("{base}/fruit"), format!("{base}/vanishing")];
    let seeds: Vec<&str> = seeds.iter().map(String::as_str).collect();

    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let run = |safety: Safety| crawler_with_safety(index.clone(), store.clone(), CrawlConfig::default(), safety);

    let first = crawl(run(Safety::default()), &seeds).await;
    assert_eq!(first.indexed, 2, "{first:?}");
    assert_eq!(hits(&index, "apples", &[]).len(), 1);
    assert_eq!(hits(&index, "vanishing", &[]).len(), 1);

    // The page is gone and "apples" is now a blocked term; seeds are
    // revisited at once even though they are fresh.
    gone.store(true, Ordering::SeqCst);
    let second = crawl(run(Safety::from_lists("", "apples")), &seeds).await;
    assert_eq!((second.blocked, second.failed), (1, 1), "{second:?}");
    assert!(hits(&index, "apples", &[]).is_empty(), "a newly blocked page leaves the index");
    assert!(hits(&index, "vanishing", &[]).is_empty(), "a 404 leaves the index");

    // Blocked for good: seeding it again does not fetch it.
    let third = crawl(run(Safety::default()), &seeds[..1]).await;
    assert_eq!((third.fetched, third.indexed), (0, 0), "{third:?}");
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
        i2p_proxies: vec!["http://127.0.0.1:9".parse().unwrap()],
        ..GatewayConfig::default()
    })
    .unwrap();
    let error = gateway.fetch(&Source::parse("http://stats.i2p/").unwrap(), &Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("is the router running"), "{error}");
    let error =
        gateway.fetch(&Source::parse(&format!("http://{ONION}/")).unwrap(), &Default::default()).await.unwrap_err();
    assert!(error.to_string().contains("Tor is disabled"), "{error}");
}

/// Two sites on one server, told apart by name: 127.0.0.1 and localhost.
/// The first is a chain four pages deep whose last page links to the
/// second, and has a page with thirty links besides.
async fn serve_two_sites() -> (String, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let many: String = (0..30).map(|i| format!(r#"<a href="/m/{i}">{i}</a> "#)).collect();
    let app = Router::new()
        .route("/", get(|| async { Html(r#"<title>start</title><a href="/1">1</a>"#) }))
        .route("/1", get(|| async { Html(r#"<title>first floor</title><a href="/2">2</a>"#) }))
        .route("/2", get(|| async { Html(r#"<title>second floor</title><a href="/3">3</a>"#) }))
        .route(
            "/3",
            get(move || async move {
                Html(format!(r#"<title>third floor</title><a href="http://localhost:{port}/far">elsewhere</a>"#))
            }),
        )
        .route("/far", get(|| async { Html(r#"<title>far away</title><a href="/beyond">on</a>"#) }))
        .route("/beyond", get(|| async { Html("<title>beyond the hills</title>") }))
        .route("/many", get(move || async move { Html(format!("<title>many links</title>{many}")) }))
        .route("/m/{n}", get(|| async { Html("<title>one of many</title>") }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}/"), port)
}

#[tokio::test]
async fn goes_deep_within_sites_and_on_across_them() {
    let (base, _) = serve_two_sites().await;
    let cfg = |max_depth| CrawlConfig {
        max_depth,
        max_site_depth: 4,
        max_site_pages: 10,
        clearnet_same_site_only: false,
        ..CrawlConfig::default()
    };
    // Pages within a site do not use up the hops between sites: four pages
    // in, the link to the other site is its first hop, and that site is
    // crawled in turn.
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    crawl(crawler_with(index.clone(), store.clone(), cfg(1)), &[&base]).await;
    assert_eq!(hits(&index, "\"third floor\"", &[]).len(), 1);
    assert_eq!(hits(&index, "\"far away\"", &[]).len(), 1, "the other site, reached from four pages in");
    assert_eq!(hits(&index, "\"beyond the hills\"", &[]).len(), 1, "and a page inside it");

    // A page with thirty links: the site keeps ten addresses, the page
    // itself among them.
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    crawl(crawler_with(Arc::new(SearchIndex::in_memory().unwrap()), store.clone(), cfg(1)), &[&format!("{base}many")])
        .await;
    assert_eq!(store.known().unwrap(), 10);

    // With no hops between sites allowed, the crawl stays on the first.
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    crawl(crawler_with(index.clone(), Arc::new(CrawlStore::in_memory().unwrap()), cfg(0)), &[&base]).await;
    assert_eq!(hits(&index, "\"third floor\"", &[]).len(), 1);
    assert!(hits(&index, "\"far away\"", &[]).is_empty());
}

const CID: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
const OTHER_CID: &str = "bafkqaaa";
const PLANTED_CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
const FREESITE: &str =
    "USK@AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA,BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB,AQACAAE/site/1/";

/// An IPFS gateway, a web page linking into IPFS and a freesite linking a CID
/// of its own, all on one server: the gateway under `/ipfs/`, FProxy under
/// `/USK@`. Returns the server and the gateway paths asked for.
async fn serve_ipfs_and_its_neighbours() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    let app = Router::new()
        .route("/", get(|| async { Html(format!(r#"<title>Links</title><a href="ipfs://{CID}/">on IPFS</a>"#)) }))
        .route(
            &format!("/{FREESITE}"),
            get(|| async { Html(format!(r#"<title>A freesite</title><a href="ipfs://{PLANTED_CID}/">a CID</a>"#)) }),
        )
        .fallback(move |uri: axum::http::Uri| {
            log.lock().unwrap().push(uri.path().to_owned());
            async move {
                let page = match uri.path() {
                    p if p == format!("/ipfs/{CID}/") => {
                        format!(
                            r#"<title>IPFS home</title><a href="page.html">p</a> <a href="/ipfs/{OTHER_CID}/">o</a>"#
                        )
                    }
                    p if p == format!("/ipfs/{CID}/page.html") => {
                        "<title>Walnuts</title><p>all about walnuts</p>".into()
                    }
                    p if p == format!("/ipfs/{OTHER_CID}/") => "<title>Second root</title><p>pecans</p>".into(),
                    _ => return StatusCode::NOT_FOUND.into_response(),
                };
                // An older gateway's header.
                ([(CONTENT_TYPE, "text/html"), (axum::http::HeaderName::from_static("x-ipfs-path"), uri.path())], page)
                    .into_response()
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, asked)
}

#[tokio::test]
async fn crawls_ipfs_from_the_web_but_not_from_anonymous_pages() {
    let (base, asked) = serve_ipfs_and_its_neighbours().await;
    let node = vec![base.parse().unwrap()];
    let gateway = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: node.clone(),
        ipfs_gateways: node,
        allow_private_addresses: true,
        ..GatewayConfig::default()
    })
    .unwrap();
    let no_delay = Politeness { concurrency: 4, site_delay: Duration::ZERO };
    let cfg = CrawlConfig {
        politeness: HashMap::from([
            (Network::Clearnet, no_delay),
            (Network::Hyphanet, no_delay),
            (Network::Ipfs, no_delay),
        ]),
        commit_interval: Duration::from_millis(100),
        ..CrawlConfig::default()
    };
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let crawler = Crawler::new(Arc::new(gateway), index.clone(), Arc::new(Safety::default()), store, cfg);
    let stats = crawl(crawler, &[&base, &format!("freenet:{FREESITE}")]).await;

    // The web page's link into IPFS, the page beside it and the other root
    // it links as a gateway path.
    assert_eq!(stats.indexed, 5, "{stats:?}");
    let walnuts = hits(&index, "walnuts", &[Network::Ipfs]);
    assert_eq!(walnuts.len(), 1);
    assert_eq!(walnuts[0].routes[0].url, format!("ipfs://{CID}/page.html"));
    assert_eq!(hits(&index, "pecans", &[Network::Ipfs]).len(), 1);
    // The freesite's CID was not fetched: the node would have asked its peers
    // for it from its own address, which the freesite's author could watch.
    assert!(!asked.lock().unwrap().iter().any(|p| p.contains(PLANTED_CID)), "{:?}", asked.lock().unwrap());
    assert_eq!(hits(&index, "freesite", &[Network::Hyphanet]).len(), 1);
}

/// A forum with what forums have besides threads: people, sign-in and search
/// pages, a post's address with a parameter beside its own, a session ID in
/// its links, a bot check, and a Flarum-style page whose links are all in
/// `<noscript>`. Returns the base and the paths fetched.
async fn serve_forum() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    const SID: &str = "0123456789abcdef0123456789abcdef";
    let fetched = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = fetched.clone();
    let app = Router::new().fallback(move |uri: axum::http::Uri| {
        log.lock().unwrap().push(uri.to_string());
        async move {
            let page = match (uri.path(), uri.query()) {
                ("/", _) => format!(
                    r#"<title>Board index</title>
                    <a href="/t/walnuts/1">thread</a> <a href="/u/alice">alice</a> <a href="/login">log in</a>
                    <a href="/search?q=x">search</a> <a href="/post/1?scrollToComments=true">post</a>
                    <a href="/viewtopic.php?t=3&sid={SID}">topic</a> <a href="/viewtopic.php?t=3">topic</a>
                    <a href="/check">check</a> <a href="/flarum">flarum</a>"#
                ),
                ("/t/walnuts/1", _) => "<title>Walnut thread</title><p>all about walnuts</p>".into(),
                ("/post/1", None) => {
                    r#"<link rel="canonical" href="/post/1"><title>Pecan post</title><p>all about pecans</p>"#.into()
                }
                ("/post/1", Some(_)) => {
                    r#"<link rel="canonical" href="/post/1"><title>Pecan post</title><p>all about pecans, again</p>"#.into()
                }
                ("/viewtopic.php", Some("t=3")) => "<title>Hazel topic</title><p>all about hazelnuts</p>".into(),
                ("/check", _) => {
                    r#"<title></title><form><p>Human Verification</p><div class="h-captcha" data-sitekey="k"></div></form>"#.into()
                }
                ("/flarum", _) => r#"<title>Flarum</title><noscript>You need JavaScript.</noscript>
                    <noscript id="flarum-content"><ul><li><a href="/d/2-almonds">Almonds</a></li></ul></noscript>"#
                    .into(),
                ("/d/2-almonds", _) => "<title>Almond discussion</title><p>all about cashews</p>".into(),
                _ => return StatusCode::NOT_FOUND.into_response(),
            };
            Html(page).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, fetched)
}

#[tokio::test]
async fn crawls_a_forums_threads_and_leaves_the_rest() {
    let (base, fetched) = serve_forum().await;
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let stats = crawl(crawler(index.clone(), store, None), &[&base]).await;

    let fetched = fetched.lock().unwrap().clone();
    for path in ["/u/alice", "/login", "/search?q=x"] {
        assert!(!fetched.iter().any(|p| p == path), "{path} was fetched: {fetched:?}");
    }
    let topic_fetches = fetched.iter().filter(|p| p.starts_with("/viewtopic.php")).count();
    assert_eq!(topic_fetches, 1, "the session ID made no second address: {fetched:?}");

    // The board, the thread, the post (once, at its own address), the topic,
    // the Flarum page and the discussion only its <noscript> links to.
    assert_eq!(stats.indexed, 6, "{stats:?}");
    for word in ["walnuts", "hazelnuts", "cashews"] {
        assert_eq!(hits(&index, word, &[]).len(), 1, "{word}");
    }
    let pecans = hits(&index, "pecans", &[]);
    assert_eq!(pecans.len(), 1);
    assert_eq!(pecans[0].routes[0].url, format!("{base}post/1"));
    assert!(hits(&index, "verification", &[]).is_empty(), "the bot check is not a page");
    assert_eq!(stats.failed, 1, "the bot check is asked again later: {stats:?}");
}

/// A Gopher hole on 127.0.0.1: a root menu, a phlog menu, a text post, a
/// binary, a link to the web and a robots.txt that keeps `/secret` out.
/// Returns its port and the selectors asked for.
async fn serve_gopher() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n") {
                    let Ok(byte) = stream.read_u8().await else { return };
                    request.push(byte);
                }
                let selector = String::from_utf8_lossy(&request[..request.len() - 2]).into_owned();
                log.lock().unwrap().push(selector.clone());
                let item =
                    |kind: char, name: &str, selector: &str| format!("{kind}{name}\t{selector}\t127.0.0.1\t{port}\r\n");
                let answer = match selector.as_str() {
                    "" => [
                        "i  _/\\_  \t\terror.host\t1\r\n".to_owned(),
                        "iWelcome to the quiet hole\t\terror.host\t1\r\n".to_owned(),
                        item('1', "Phlog", "/phlog/"),
                        item('0', "Secret notes", "/secret/notes.txt"),
                        item('9', "A tarball", "/files/hole.tar.gz"),
                        "hThe web site\tURL:http://web.example/\t127.0.0.1\t70\r\n".to_owned(),
                        ".\r\n".to_owned(),
                    ]
                    .concat(),
                    "/phlog/" => [item('0', "On lichen", "/phlog/lichen.txt"), ".\r\n".to_owned()].concat(),
                    "/phlog/lichen.txt" => {
                        "On lichen\r\n\r\nLichen grows slowly on the north side of stones.\r\n".to_owned()
                    }
                    "robots.txt" => "User-agent: *\r\nDisallow: /secret\r\n".to_owned(),
                    other => format!("3'{other}' does not exist\terror.host\t1\r\n"),
                };
                let _ = stream.write_all(answer.as_bytes()).await;
            });
        }
    });
    (port, asked)
}

#[tokio::test]
async fn crawls_a_gopher_hole_and_leaves_binaries_alone() {
    let (port, asked) = serve_gopher().await;
    let index = Arc::new(SearchIndex::in_memory().unwrap());
    let store = Arc::new(CrawlStore::in_memory().unwrap());
    let no_delay = Politeness { concurrency: 4, site_delay: Duration::ZERO };
    let cfg = CrawlConfig { politeness: HashMap::from([(Network::Gopher, no_delay)]), ..CrawlConfig::default() };
    let gateway =
        Gateway::new(GatewayConfig { tor_dir: None, allow_private_addresses: true, ..GatewayConfig::default() })
            .unwrap();
    let crawler = Crawler::new(
        Arc::new(gateway),
        index.clone(),
        Arc::new(Safety::default()),
        store,
        CrawlConfig { commit_interval: Duration::from_millis(100), ..cfg },
    );
    crawl(crawler, &[&format!("gopher://127.0.0.1:{port}/")]).await;

    let post = hits(&index, "lichen stones", &[Network::Gopher]);
    assert_eq!(post.len(), 1, "{post:?}");
    assert_eq!(post[0].routes[0].url, format!("gopher://127.0.0.1:{port}/0/phlog/lichen.txt"));
    assert_eq!(post[0].title, "On lichen", "a text item's first line");
    let root = hits(&index, "quiet hole", &[Network::Gopher]);
    assert_eq!(root.first().map(|h| h.title.as_str()), Some("Welcome to the quiet hole"));
    let asked = asked.lock().unwrap().clone();
    assert!(asked.contains(&"robots.txt".to_owned()), "{asked:?}");
    assert!(!asked.iter().any(|s| s.starts_with("/secret")), "robots.txt keeps it out: {asked:?}");
    assert!(!asked.iter().any(|s| s.starts_with("/files")), "a binary is never asked for: {asked:?}");
}

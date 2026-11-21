//! The omnid daemon as a library: what `omnid serve` runs, and what the
//! Android app embeds.

pub mod api;
mod socks;

pub use socks::Access as ProxyAccess;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use omnid_core::{Magnet, Network, Source, now_secs};
use omnid_crawl::{
    CrawlConfig, CrawlControl, CrawlStore, Crawler, HARVESTED_TRUST, Job, SEED_TRUST, Safety, StatsSnapshot,
};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SCHEMA_VERSION, SearchIndex};
use omnid_net::{FetchOptions, Gateway, GatewayConfig, RouterState};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct DaemonConfig {
    pub data_dir: PathBuf,
    /// Port 0 picks a free one; [`Daemon::addr`] reports it.
    pub listen: SocketAddr,
    pub gateway: GatewayConfig,
    pub crawl: CrawlConfig,
    pub safety: Safety,
    pub seeds: Vec<Job>,
    /// Required on every API request when set; see [`api::router`].
    pub api_token: Option<String>,
    pub start_paused: bool,
    /// Where to serve the proxy (SOCKS5 and HTTP on one port), if at all, and
    /// to whom. An open one has no authentication: anything that can reach it
    /// can use it, so keep it on loopback.
    pub socks: Option<(SocketAddr, ProxyAccess)>,
    /// Serves the app's built-in browser, on a free loopback port
    /// ([`Daemon::browser_proxy_addr`]), to clients that send this password:
    /// everything through Tor or i2pd, never direct, each site on circuits of
    /// its own.
    pub browser_proxy: Option<String>,
    /// The BitTorrent DHT, for magnets' file lists and swarm sizes. `None`
    /// leaves magnets indexed from their links alone.
    pub dht: Option<DhtConfig>,
    /// Also index info hashes sampled from the whole DHT (BEP 51). This pulls
    /// in torrents nobody linked to, whatever they are; the safety gate's
    /// blocked terms are checked against their names and files.
    pub dht_harvest: bool,
    /// Keeps Ahmia's list of onion services reported for abuse in the safety
    /// gate: the last copy from the data directory at once, then fetched
    /// over Tor and refreshed daily.
    pub abuse_blocklist: bool,
}

/// Starter addresses on each overlay network, for a new index: services'
/// own onion sites and Ahmia's list of onion services (which leaves out
/// those reported for abuse), I2P's hubs and directories, and the Hyphanet
/// project's default bookmarks, its indexes of freesites among them. Each
/// answered on 2026-11-21. A network's seeds wait while its router is down.
pub const DEFAULT_SEEDS: &[&str] = &[
    // Tor: the Tor Project, the press, privacy tools and their makers.
    "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/",
    "https://www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion/",
    "http://p53lf57qovyuvwsc6xnrppyply3vtqm7l6pcobkmyqsiofyeznfu5uqd.onion/",
    "http://archivep75mbjunhxc6x4j5mwjmomyxb573v42baldlqu56ruil2oiad.onion/",
    "http://sdolvtfhatvsysc6l34d65ymdwxcujausv7k5jk4cy5ttzhjoi6fzvyd.onion/",
    "http://vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion/",
    "http://protonmailrmez3lotccipshtkleegetolb73fuirgj7r4o4vfu7ozyd.onion/",
    "http://o54hon2e2vj6c7m3aqqu6uyece65by3vgoxxhlqlsvkmacw6a7m7kiad.onion/",
    "http://qubesosfasa4zl44o4tws22di6kepyzfeqv3tg4e3ztknltfxqrymdad.onion/",
    "http://dds6qkxpwdeubwucdiaord2xgbbeyds25rbsgr73tbfpqpt4a6vjwsyd.onion/",
    "http://w5j6stm77zs6652pgsij4awcjeel3eco7kvipheu6mtr623eyyehj4yd.onion/",
    // Ahmia, a search engine for onion services, and its list of them.
    "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/",
    "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/onions/",
    // I2P: the project, its news, forum and registries, and directories.
    "http://i2p-projekt.i2p/",
    "http://planet.i2p/",
    "http://zzz.i2p/",
    "http://i2pforum.i2p/",
    "http://reg.i2p/",
    "http://inr.i2p/",
    "http://stats.i2p/",
    "http://notbob.i2p/",
    "http://isitup.i2p/",
    // Hyphanet: the node's default bookmarks, Clean Spider (an index of
    // freesites) and the Index of Indexes among them.
    "freenet:USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/",
    "freenet:USK@5ijbfKSJ4kPZTRDzq363CHteEUiSZjrO-E36vbHvnIU,ZEZqPXeuYiyokY2r0wkhJr5cy7KBH9omkuWDqSC6PLs,AQACAAE/clean-spider/547/",
    "freenet:USK@t-YmshUm43nS16QFieoZMfBdfieb9Opy0a2aMWsoH8o,7NqDX88r1R4SC~elmpW4SuKbnAOka~MKUuBufUWa2xQ,AQACAAE/jFniki.Index/21/",
    "freenet:USK@0iU87PXyodL2nm6kCpmYntsteViIbMwlJE~wlqIVvZ0,nenxGvjXDElX5RIZxMvwSnOtRzUKJYjoXEDgkhY6Ljw,AQACAAE/freenetproject-mirror/519/",
    "freenet:USK@t5zaONbYd5DvGNNSokVnDCdrIEytn9U5SSD~pYF0RTE,guWyS9aCMcywU5PFBrKsMiXs7LzwKfQlGSRi17fpffc,AQACAAE/fsng/58/",
    "freenet:USK@8r-uSRcJPkAr-3v3YJR16OCx~lyV2XOKsiG4MOQQBMM,P42IgNemestUdaI7T6z3Og6P-Hi7g9U~e37R3kWGVj8,AQACAAE/freesite-HOWTO/4/",
];

/// [`DEFAULT_SEEDS`] as crawl jobs.
pub fn default_seeds() -> Vec<Job> {
    DEFAULT_SEEDS
        .iter()
        .filter_map(|s| Source::parse(s).ok())
        .map(|source| Job { source, depth: 0, site_depth: 0, trust: SEED_TRUST })
        .collect()
}

/// Ahmia's list of onion services reported for abuse, as MD5 digests of
/// their hostnames, from its own onion site.
const AHMIA_BLOCKLIST: &str = "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/blacklist/banned/";
const BLOCKLIST_FILE: &str = "ahmia-banned.txt";
/// A list shorter than this is a broken answer, not a list: keep the one held.
const MIN_BLOCKLIST: usize = 1_000;
const BLOCKLIST_REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
const BLOCKLIST_RETRY: Duration = Duration::from_secs(30 * 60);

/// Puts the last copy of Ahmia's blocklist from `data_dir` into the safety
/// gate. Returns how many sites it holds.
pub fn load_blocklist(safety: &Safety, data_dir: &Path) -> usize {
    match std::fs::read_to_string(data_dir.join(BLOCKLIST_FILE)) {
        Ok(list) => safety.set_published(&list),
        Err(_) => 0,
    }
}

/// Fetches Ahmia's blocklist over Tor, puts it into the safety gate and
/// keeps a copy in `data_dir`. Returns how many sites it holds.
pub async fn refresh_blocklist(gateway: &Gateway, safety: &Safety, data_dir: &Path) -> Result<usize> {
    let source = Source::parse(AHMIA_BLOCKLIST)?;
    let opts = FetchOptions {
        max_bytes: 16 << 20,
        accept: |content_type| content_type.is_some_and(|t| t.starts_with("text/plain")),
        ..FetchOptions::default()
    };
    let page = gateway.fetch(&source, &opts).await.map_err(|e| anyhow!("{e}"))?;
    anyhow::ensure!(page.status == 200 && !page.truncated, "Ahmia answered {} for its blocklist", page.status);
    let list = String::from_utf8_lossy(page.body.as_deref().unwrap_or_default()).into_owned();
    let digests = list.lines().filter(|l| l.trim().len() == 32).count();
    anyhow::ensure!(digests >= MIN_BLOCKLIST, "Ahmia's blocklist had {digests} entries; keeping the one held");
    let held = safety.set_published(&list);
    std::fs::write(data_dir.join(BLOCKLIST_FILE), &list).context("saving the blocklist")?;
    Ok(held)
}

/// Keeps the blocklist current: refreshed daily, retried half-hourly when
/// Ahmia cannot be reached.
async fn keep_blocklist(gateway: Arc<Gateway>, safety: Arc<Safety>, data_dir: PathBuf, shutdown: CancellationToken) {
    loop {
        let wait = match refresh_blocklist(&gateway, &safety, &data_dir).await {
            Ok(sites) => {
                tracing::info!(sites, "abuse blocklist refreshed from Ahmia");
                BLOCKLIST_REFRESH
            }
            Err(e) => {
                tracing::warn!("abuse blocklist: {e:#}; trying again in 30 minutes");
                BLOCKLIST_RETRY
            }
        };
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// A running daemon: routers, crawler and the local API.
pub struct Daemon {
    addr: SocketAddr,
    socks: Option<SocketAddr>,
    browser_proxy: Option<SocketAddr>,
    gateway: Arc<Gateway>,
    control: Arc<CrawlControl>,
    shutdown: CancellationToken,
    crawl: JoinHandle<StatsSnapshot>,
    server: JoinHandle<std::io::Result<()>>,
}

/// Starts everything and returns once the API is listening. Call inside a
/// multi-threaded Tokio runtime.
pub async fn start(cfg: DaemonConfig) -> Result<Daemon> {
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let gateway = Arc::new(Gateway::new(cfg.gateway).map_err(|e| anyhow!("starting the gateway: {e}"))?);
    let index = Arc::new(open_index(&cfg.data_dir)?);
    let store = Arc::new(open_store(&cfg.data_dir)?);
    // The seeds are the operator's choice: a published blocklist does not
    // overrule it, and what one blocked on those sites before is due again.
    let mut safety = cfg.safety;
    let seeds: Vec<&Source> = cfg.seeds.iter().map(|job| &job.source).collect();
    let defaults = default_seeds();
    let chosen: Vec<String> =
        seeds.into_iter().chain(defaults.iter().map(|job| &job.source)).map(Source::site_key).collect();
    safety.choose_sites(&chosen);
    let unblocked = store.unblock_sites(chosen.iter().filter(|site| safety.operator_allows(site)), now_secs())?;
    if unblocked > 0 {
        tracing::info!(unblocked, "addresses on seed sites that a blocklist had refused are due again");
    }
    let safety = Arc::new(safety);
    if cfg.abuse_blocklist {
        let held = load_blocklist(&safety, &cfg.data_dir);
        tracing::info!(sites = held, "abuse blocklist loaded from the last copy");
    }
    tracing::info!(denylist = safety.denylist_len(), blocked_terms = safety.blocked_terms_len(), "safety gate loaded");

    let dht = match cfg.dht {
        Some(dht_cfg) => match Dht::start(dht_cfg).await {
            Ok(dht) => Some(dht),
            Err(e) => {
                tracing::warn!("DHT disabled: {e}");
                None
            }
        },
        None => None,
    };
    let mut crawler = Crawler::new(gateway.clone(), index.clone(), safety.clone(), store.clone(), cfg.crawl);
    if let Some(dht) = &dht {
        crawler = crawler.with_dht(dht.clone());
    }
    let control = crawler.control();
    control.set_paused(cfg.start_paused);
    let (jobs, rx) = mpsc::channel(1024);
    let shutdown = CancellationToken::new();
    let crawl = tokio::spawn(crawler.run(rx, shutdown.clone()));
    if let (Some(dht), true) = (&dht, cfg.dht_harvest) {
        tokio::spawn(harvest(dht.clone(), store.clone(), shutdown.clone()));
    }
    tokio::spawn(watch_routers(gateway.clone(), store, shutdown.clone()));
    if cfg.abuse_blocklist {
        tokio::spawn(keep_blocklist(gateway.clone(), safety.clone(), cfg.data_dir.clone(), shutdown.clone()));
    }
    let seeder = jobs.clone();
    tokio::spawn(async move {
        for job in cfg.seeds {
            if seeder.send(job).await.is_err() {
                break;
            }
        }
    });

    let listener =
        tokio::net::TcpListener::bind(cfg.listen).await.with_context(|| format!("binding {}", cfg.listen))?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && cfg.api_token.is_none() {
        tracing::warn!("the API is reachable beyond this machine and has no token; set one");
    }
    let socks = match cfg.socks {
        Some((addr, access)) => {
            let listener =
                tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding the proxy to {addr}"))?;
            let bound = listener.local_addr()?;
            let private = !matches!(access, ProxyAccess::Open);
            tokio::spawn(socks::serve(listener, gateway.clone(), Arc::new(access), shutdown.clone()));
            tracing::info!(private, "SOCKS5 and HTTP proxy on {bound}");
            Some(bound)
        }
        None => None,
    };
    let browser_proxy = match cfg.browser_proxy {
        Some(password) => {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .context("binding the browser's proxy")?;
            let bound = listener.local_addr()?;
            let access = Arc::new(socks::Access::Browser { password });
            tokio::spawn(socks::serve(listener, gateway.clone(), access, shutdown.clone()));
            tracing::info!("browser proxy on {bound}");
            Some(bound)
        }
        None => None,
    };
    let state = api::AppState { gateway: gateway.clone(), index, control: control.clone(), jobs, safety, dht };
    let router = api::router(state, addr, cfg.api_token);
    let stop = shutdown.clone().cancelled_owned();
    let server = tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop).await });
    tracing::info!("omnid {} listening on http://{addr}", env!("CARGO_PKG_VERSION"));
    Ok(Daemon { addr, socks, browser_proxy, gateway, control, shutdown, crawl, server })
}

impl Daemon {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn socks_addr(&self) -> Option<SocketAddr> {
        self.socks
    }

    pub fn browser_proxy_addr(&self) -> Option<SocketAddr> {
        self.browser_proxy
    }

    /// New Tor circuits for every site the browser visits from now on.
    pub fn new_identity(&self) {
        self.gateway.new_identity();
    }

    pub fn control(&self) -> &Arc<CrawlControl> {
        &self.control
    }

    /// The gateway, for what the app asks of the network itself (downloads).
    pub fn gateway(&self) -> &Arc<Gateway> {
        &self.gateway
    }

    /// Stops accepting requests and crawling; [`wait`](Self::wait) returns
    /// once the index is committed.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// For stopping the daemon from another task.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn wait(self) -> Result<StatsSnapshot> {
        self.server.await??;
        // The server stopping for any reason stops the crawl too.
        self.shutdown.cancel();
        Ok(self.crawl.await?)
    }
}

/// How often the daemon looks whether the I2P router and Hyphanet node are up.
const ROUTER_CHECK: Duration = Duration::from_secs(30);

/// When the I2P router or the Hyphanet node comes up (the i2pd app started,
/// a node set), what was put off for want of it is crawled now instead of
/// after its deferral.
async fn watch_routers(gateway: Arc<Gateway>, store: Arc<CrawlStore>, shutdown: CancellationToken) {
    let mut was_ready = HashSet::new();
    loop {
        let ready: HashSet<Network> =
            gateway.status().await.into_iter().filter(|r| r.state == RouterState::Ready).map(|r| r.network).collect();
        for network in [Network::I2p, Network::Hyphanet] {
            if ready.contains(&network) && !was_ready.contains(&network) {
                let store = store.clone();
                match tokio::task::spawn_blocking(move || store.retry_deferred(network, now_secs())).await {
                    Ok(Ok(0)) => {}
                    Ok(Ok(n)) => tracing::info!(%network, retried = n, "router up: crawling what waited for it"),
                    Ok(Err(e)) => tracing::warn!(%network, "retrying deferred addresses: {e}"),
                    Err(e) => tracing::warn!(%network, "retrying deferred addresses: {e}"),
                }
            }
        }
        was_ready = ready;
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(ROUTER_CHECK) => {}
        }
    }
}

/// Writes sampled info hashes to the crawl store, where the crawler picks
/// them up like any other magnet.
async fn harvest(dht: Dht, store: Arc<CrawlStore>, shutdown: CancellationToken) {
    tracing::info!("harvesting info hashes from the DHT");
    let mut hashes = dht.harvest();
    let mut batch = Vec::new();
    loop {
        let received = tokio::select! {
            _ = shutdown.cancelled() => return,
            n = hashes.recv_many(&mut batch, 256) => n,
        };
        if received == 0 {
            return;
        }
        let jobs: Vec<_> = batch
            .drain(..)
            .map(|info_hash| Job {
                source: Source::Magnet(Magnet { info_hash, name: None }),
                depth: 0,
                site_depth: 0,
                trust: HARVESTED_TRUST,
            })
            .collect();
        let store = store.clone();
        match tokio::task::spawn_blocking(move || store.discover(&jobs, now_secs())).await {
            Ok(Ok(added)) => tracing::debug!(added, "harvested"),
            Ok(Err(e)) => tracing::error!("crawl store: {e}"),
            Err(e) => tracing::error!("harvest write died: {e}"),
        }
    }
}

/// The index lives in a directory named for its schema version, so an
/// upgrade builds a new index next to the old one.
pub fn index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("index-v{SCHEMA_VERSION}"))
}

/// Opens the index, first carrying the previous version's pages over if
/// this version has no index yet.
pub fn open_index(data_dir: &Path) -> Result<SearchIndex> {
    let dir = index_dir(data_dir);
    let previous = data_dir.join(format!("index-v{}", SCHEMA_VERSION - 1));
    if !dir.exists() && previous.exists() {
        migrate_index(&previous, &dir).with_context(|| format!("migrating {}", previous.display()))?;
    }
    SearchIndex::open(&dir).with_context(|| format!("opening {} (is `omnid serve` already running?)", dir.display()))
}

/// Builds the new index aside and renames it into place once committed, so
/// an interrupted upgrade starts over and never leaves half an index.
fn migrate_index(previous: &Path, dir: &Path) -> Result<()> {
    let staging = dir.with_extension("migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let copied = SearchIndex::open(&staging)?.migrate_from(previous)?;
    std::fs::rename(&staging, dir)?;
    std::fs::remove_dir_all(previous)?;
    tracing::info!(copied, "carried the index over from {}", previous.display());
    Ok(())
}

pub fn open_store(data_dir: &Path) -> Result<CrawlStore> {
    let path = data_dir.join("crawl.sqlite");
    CrawlStore::open(&path).with_context(|| format!("opening {}", path.display()))
}

/// One address per line; `#` starts a comment.
pub fn read_seeds(path: &Path) -> Result<Vec<Job>> {
    let list = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    list.lines()
        .enumerate()
        .map(|(n, line)| (n + 1, line.split('#').next().unwrap_or_default().trim()))
        .filter(|(_, line)| !line.is_empty())
        .map(|(n, line)| {
            let source = Source::parse(line).with_context(|| format!("{}:{n}", path.display()))?;
            Ok(Job { source, depth: 0, site_depth: 0, trust: SEED_TRUST })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_somewhere_on_every_overlay_network() {
        let seeds = default_seeds();
        assert_eq!(seeds.len(), DEFAULT_SEEDS.len(), "every seed parses");
        for network in [Network::Onion, Network::I2p, Network::Hyphanet] {
            assert!(seeds.iter().any(|j| j.source.network() == network), "{network} has seeds");
        }
        assert!(!seeds.iter().any(|j| j.source.network() == Network::Clearnet), "overlay sites only");
        // Added once: a store that has them gains nothing more.
        let store = CrawlStore::in_memory().unwrap();
        assert_eq!(store.discover(&seeds, 0).unwrap(), seeds.len());
        assert_eq!(store.discover(&default_seeds(), 0).unwrap(), 0);
    }

    #[test]
    fn loads_the_last_copy_of_the_blocklist() {
        let dir = std::env::temp_dir().join(format!("omnid-blocklist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let safety = Safety::default();
        assert_eq!(load_blocklist(&safety, &dir), 0, "no copy yet");
        let list: String = (0..MIN_BLOCKLIST).map(|i| format!("{i:032x}\n")).collect();
        std::fs::write(dir.join(BLOCKLIST_FILE), &list).unwrap();
        assert_eq!(load_blocklist(&safety, &dir), MIN_BLOCKLIST);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn page(url: &str) -> omnid_core::IndexedDoc {
        omnid_core::IndexedDoc {
            url: url.into(),
            network: omnid_core::Network::Clearnet,
            site: "a.example".into(),
            title: "kept across versions".into(),
            description: String::new(),
            keywords: vec![],
            body: String::new(),
            content_hash: url.into(),
            info_hash: None,
            files: vec![],
            first_seen: 1,
            last_seen: 1,
            liveness: 1.0,
            trust: 0.5,
            lang: None,
        }
    }

    #[test]
    fn carries_the_previous_index_over_once() {
        let data = tempfile::tempdir().unwrap();
        let previous = data.path().join(format!("index-v{}", SCHEMA_VERSION - 1));
        let staging = index_dir(data.path()).with_extension("migrating");
        {
            let old = SearchIndex::open(&previous).unwrap();
            old.upsert(&page("http://a.example/")).unwrap();
            old.commit().unwrap();
        }
        // What an upgrade killed halfway leaves behind.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("meta.json"), "{").unwrap();

        let index = open_index(data.path()).unwrap();
        assert!(index.get("http://a.example/").unwrap().is_some());
        assert!(!previous.exists(), "the old index goes once it is copied");
        assert!(!staging.exists());
        drop(index);
        assert_eq!(open_index(data.path()).unwrap().num_docs(), 1, "the next start opens it as it is");
    }
}

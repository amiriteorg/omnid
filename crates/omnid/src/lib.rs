//! The omnid daemon as a library: what `omnid serve` runs, and what the
//! Android app embeds.

pub mod api;
mod socks;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use omnid_core::{Magnet, Source, now_secs};
use omnid_crawl::{
    CrawlConfig, CrawlControl, CrawlStore, Crawler, HARVESTED_TRUST, Job, SEED_TRUST, Safety, StatsSnapshot,
};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SCHEMA_VERSION, SearchIndex};
use omnid_net::{Gateway, GatewayConfig};
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
    /// Where to serve the SOCKS5 proxy, if at all. It has no authentication:
    /// anything that can reach it can use it, so keep it on loopback.
    pub socks: Option<SocketAddr>,
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
    let safety = Arc::new(cfg.safety);
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
        tokio::spawn(harvest(dht.clone(), store, shutdown.clone()));
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
        Some(addr) => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding the SOCKS proxy to {addr}"))?;
            let bound = listener.local_addr()?;
            tokio::spawn(socks::serve(listener, gateway.clone(), Arc::new(socks::Access::Open), shutdown.clone()));
            tracing::info!("SOCKS5 proxy on {bound}");
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
            Ok(Job { source, depth: 0, trust: SEED_TRUST })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

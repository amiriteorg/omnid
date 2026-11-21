//! The omnid daemon as a library: what `omnid serve` runs, and what the
//! Android app embeds.

pub mod api;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use omnid_core::Source;
use omnid_crawl::{CrawlConfig, CrawlControl, CrawlStore, Crawler, Job, SEED_TRUST, Safety, StatsSnapshot};
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
}

/// A running daemon: routers, crawler and the local API.
pub struct Daemon {
    addr: SocketAddr,
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

    let crawler = Crawler::new(gateway.clone(), index.clone(), safety.clone(), store, cfg.crawl);
    let control = crawler.control();
    control.set_paused(cfg.start_paused);
    let (jobs, rx) = mpsc::channel(1024);
    let shutdown = CancellationToken::new();
    let crawl = tokio::spawn(crawler.run(rx, shutdown.clone()));
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
    let state = api::AppState { gateway, index, control: control.clone(), jobs, safety };
    let router = api::router(state, addr, cfg.api_token);
    let stop = shutdown.clone().cancelled_owned();
    let server = tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop).await });
    tracing::info!("omnid {} listening on http://{addr}", env!("CARGO_PKG_VERSION"));
    Ok(Daemon { addr, control, shutdown, crawl, server })
}

impl Daemon {
    pub fn addr(&self) -> SocketAddr {
        self.addr
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

/// The index lives in a directory named for its schema version, so an
/// upgrade starts a fresh index next to the old one.
pub fn index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("index-v{SCHEMA_VERSION}"))
}

pub fn open_index(data_dir: &Path) -> Result<SearchIndex> {
    let dir = index_dir(data_dir);
    SearchIndex::open(&dir).with_context(|| format!("opening {} (is `omnid serve` already running?)", dir.display()))
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

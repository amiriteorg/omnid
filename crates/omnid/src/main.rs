//! omnid: one daemon for Tor, I2P, Hyphanet, magnets and the clearnet, with
//! one search index over all of them.

mod api;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, Crawler, Job, SEED_TRUST, Safety, extract, is_indexable_type};
use omnid_index::{SCHEMA_VERSION, SearchIndex, SearchQuery};
use omnid_net::{FetchOptions, Gateway, GatewayConfig};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use url::Url;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct Common {
    /// Where the index and Tor state live [default: the platform data dir]
    #[arg(long, env = "OMNID_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
    /// Do not start Tor
    #[arg(long, global = true)]
    no_tor: bool,
    /// i2pd's HTTP proxy
    #[arg(long, env = "OMNID_I2P_PROXY", default_value = "http://127.0.0.1:4444", global = true)]
    i2p_proxy: Url,
    /// The Hyphanet node's FProxy
    #[arg(long, env = "OMNID_FPROXY", default_value = "http://127.0.0.1:8888", global = true)]
    fproxy: Url,
    /// Let clearnet fetches reach loopback and private networks
    #[arg(long, global = true)]
    allow_private_addresses: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: routers, crawler and the local API
    Serve(ServeArgs),
    /// Crawl from seeds until there is nothing left to crawl, then exit
    Crawl(CrawlArgs),
    /// Fetch one address through the gateway and describe the response
    Fetch {
        /// A URL, bare host, freenet: key or magnet link
        address: String,
    },
    /// Search the local index
    Search {
        query: String,
        /// Only these networks (repeatable): clearnet, onion, i2p, hyphanet, magnet
        #[arg(long = "network", short)]
        networks: Vec<Network>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, env = "OMNID_LISTEN", default_value = "127.0.0.1:7171")]
    listen: SocketAddr,
    #[command(flatten)]
    crawl: CrawlOptions,
}

#[derive(Args)]
struct CrawlArgs {
    /// Addresses to start from, in addition to --seeds
    addresses: Vec<String>,
    #[command(flatten)]
    crawl: CrawlOptions,
}

#[derive(Args)]
struct CrawlOptions {
    /// Seed list: one address per line, `#` starts a comment
    #[arg(long, env = "OMNID_SEEDS")]
    seeds: Option<PathBuf>,
    /// Sites never to fetch: hostnames, keys, magnets, or MD5 hex of a hostname
    #[arg(long, env = "OMNID_DENYLIST")]
    denylist: Option<PathBuf>,
    /// Terms that keep a page out of the index, one per line
    #[arg(long, env = "OMNID_BLOCKED_TERMS")]
    blocked_terms: Option<PathBuf>,
    /// Hops to follow from a seed
    #[arg(long, default_value_t = 3)]
    max_depth: u32,
    /// Stop after this many pages
    #[arg(long)]
    max_pages: Option<u64>,
    /// Follow clearnet links to other sites, not only within the linking site
    #[arg(long)]
    follow_offsite: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tantivy=warn")))
        .init();
    let cli = Cli::parse();
    let data_dir = match cli.common.data_dir.clone() {
        Some(dir) => dir,
        None => dirs::data_dir().context("no platform data directory; pass --data-dir")?.join("omnid"),
    };
    match cli.command {
        Command::Serve(args) => serve(&cli.common, &data_dir, args).await,
        Command::Crawl(args) => crawl(&cli.common, &data_dir, args).await,
        Command::Fetch { address } => fetch(&cli.common, &data_dir, &address).await,
        Command::Search { query, networks, limit, json } => search(&data_dir, query, networks, limit, json),
    }
}

async fn serve(common: &Common, data_dir: &Path, args: ServeArgs) -> Result<()> {
    let gateway = Arc::new(gateway(common, data_dir)?);
    let index = Arc::new(open_index(data_dir)?);
    let (crawler, safety, seeds) = crawler(&args.crawl, gateway.clone(), index.clone())?;
    let stats = crawler.stats();
    let (jobs, rx) = mpsc::channel(1024);
    let shutdown = CancellationToken::new();
    let crawl = tokio::spawn(crawler.run(rx, shutdown.clone()));
    for job in seeds {
        jobs.send(job).await.context("crawler stopped")?;
    }

    let listener =
        tokio::net::TcpListener::bind(args.listen).await.with_context(|| format!("binding {}", args.listen))?;
    if !args.listen.ip().is_loopback() {
        tracing::warn!("the API is reachable beyond this machine and has no authentication");
    }
    tracing::info!("omnid {} listening on http://{}", env!("CARGO_PKG_VERSION"), args.listen);
    let state = api::AppState { gateway, index, stats, jobs, safety };
    let stop = shutdown.clone();
    axum::serve(listener, api::router(state, args.listen))
        .with_graceful_shutdown(async move {
            terminated().await;
            stop.cancel();
        })
        .await?;

    shutdown.cancel();
    let stats = crawl.await?;
    tracing::info!(?stats, "stopped");
    Ok(())
}

async fn crawl(common: &Common, data_dir: &Path, args: CrawlArgs) -> Result<()> {
    let gateway = Arc::new(gateway(common, data_dir)?);
    let index = Arc::new(open_index(data_dir)?);
    let (crawler, _, mut seeds) = crawler(&args.crawl, gateway, index)?;
    for address in &args.addresses {
        let source = Source::parse(address).with_context(|| format!("seed `{address}`"))?;
        seeds.push(Job { source, depth: 0, trust: SEED_TRUST });
    }
    anyhow::ensure!(!seeds.is_empty(), "nothing to crawl: pass addresses or --seeds");

    let (jobs, rx) = mpsc::channel(seeds.len());
    for job in seeds {
        jobs.try_send(job).expect("channel is sized for the seeds");
    }
    drop(jobs);

    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        terminated().await;
        stop.cancel();
    });
    let stats = crawler.stats();
    let progress = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.tick().await;
        loop {
            tick.tick().await;
            tracing::info!(stats = ?stats.snapshot(), "crawling");
        }
    });
    let stats = crawler.run(rx, shutdown).await;
    progress.abort();
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

async fn fetch(common: &Common, data_dir: &Path, address: &str) -> Result<()> {
    let source = Source::parse(address)?;
    let gateway = gateway(common, data_dir)?;
    // The same content-type gate as the crawler: media is never downloaded.
    let opts = FetchOptions { accept: is_indexable_type, ..FetchOptions::default() };
    let page = gateway.fetch(&source, &opts).await?;
    println!("network       {}", page.source.network());
    println!("address       {}", page.source);
    println!("status        {}", page.status);
    println!("content-type  {}", page.content_type.as_deref().unwrap_or("-"));
    if let Some(redirect) = &page.redirect {
        println!("redirect      {redirect}");
    }
    match &page.body {
        Some(body) => {
            let extracted = extract::extract(page.content_type.as_deref(), body);
            println!("bytes         {}{}", body.len(), if page.truncated { " (truncated)" } else { "" });
            println!("title         {}", extracted.title);
            println!("links         {}", extracted.links.len());
        }
        None if page.redirect.is_none() => println!("body          not downloaded (not a text type)"),
        None => {}
    }
    Ok(())
}

fn search(data_dir: &Path, text: String, networks: Vec<Network>, limit: usize, json: bool) -> Result<()> {
    let dir = index_dir(data_dir);
    let index = SearchIndex::open_read_only(&dir)
        .with_context(|| format!("opening {}; run `omnid crawl` or `omnid serve` first", dir.display()))?;
    let hits = index.search(&SearchQuery { text, networks, limit })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("no results");
    }
    for hit in hits {
        println!("{}", hit.title);
        if !hit.description.is_empty() {
            println!("  {}", hit.description);
        }
        for route in hit.routes {
            println!("  {:<9} {}", route.network.as_str(), route.url);
        }
        println!();
    }
    Ok(())
}

fn gateway(common: &Common, data_dir: &Path) -> Result<Gateway> {
    Gateway::new(GatewayConfig {
        tor_dir: (!common.no_tor).then(|| data_dir.join("tor")),
        i2p_proxy: Some(common.i2p_proxy.clone()),
        fproxy: Some(common.fproxy.clone()),
        allow_private_addresses: common.allow_private_addresses,
        ..GatewayConfig::default()
    })
    .map_err(|e| anyhow!("starting the gateway: {e}"))
}

fn index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("index-v{SCHEMA_VERSION}"))
}

fn open_index(data_dir: &Path) -> Result<SearchIndex> {
    let dir = index_dir(data_dir);
    SearchIndex::open(&dir).with_context(|| format!("opening {} (is `omnid serve` already running?)", dir.display()))
}

fn crawler(
    opts: &CrawlOptions,
    gateway: Arc<Gateway>,
    index: Arc<SearchIndex>,
) -> Result<(Crawler, Arc<Safety>, Vec<Job>)> {
    let safety = Arc::new(
        Safety::load(opts.denylist.as_deref(), opts.blocked_terms.as_deref()).context("loading safety lists")?,
    );
    tracing::info!(denylist = safety.denylist_len(), blocked_terms = safety.blocked_terms_len(), "safety gate loaded");
    let seeds = match &opts.seeds {
        Some(path) => read_seeds(path)?,
        None => vec![],
    };
    let cfg = CrawlConfig {
        max_depth: opts.max_depth,
        max_pages: opts.max_pages,
        clearnet_same_site_only: !opts.follow_offsite,
        ..CrawlConfig::default()
    };
    Ok((Crawler::new(gateway, index, safety.clone(), cfg), safety, seeds))
}

fn read_seeds(path: &Path) -> Result<Vec<Job>> {
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

/// Ctrl-C, or SIGTERM from a service manager.
async fn terminated() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("installing a SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

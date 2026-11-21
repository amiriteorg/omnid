//! omnid: one daemon for Tor, I2P, Hyphanet, magnets and the clearnet, with
//! one search index over all of them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use omnid::DaemonConfig;
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, Crawler, Job, SEED_TRUST, Safety, extract, is_indexable_type};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SearchIndex, SearchQuery};
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
    /// Reach the clearnet through Tor exits instead of directly
    #[arg(long, global = true)]
    clearnet_via_tor: bool,
    /// Do not join the BitTorrent DHT (magnets are then indexed from their links alone)
    #[arg(long, global = true)]
    no_dht: bool,
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
        /// Languages to rank first, best first, e.g. `en` or `de,en`
        /// [default: from LANG]
        #[arg(long, value_delimiter = ',')]
        lang: Vec<String>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, env = "OMNID_LISTEN", default_value = "127.0.0.1:7171")]
    listen: SocketAddr,
    /// Require this token on every API request (bearer or `omnid_token` cookie)
    #[arg(long, env = "OMNID_API_TOKEN", hide_env_values = true)]
    api_token: Option<String>,
    /// Start with crawling paused; resume with POST /api/crawl
    #[arg(long)]
    paused: bool,
    /// Serve a SOCKS5 proxy here (e.g. 127.0.0.1:7172) so browsers can open
    /// .onion and .i2p addresses through omnid; use remote DNS (socks5h)
    #[arg(long, env = "OMNID_SOCKS")]
    socks: Option<SocketAddr>,
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
    /// Also index info hashes sampled from the whole DHT (BEP 51): torrents
    /// nobody linked to, whatever they are. Use a blocked-terms list.
    #[arg(long)]
    dht_harvest: bool,
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
        Command::Search { query, networks, limit, lang, json } => search(&data_dir, query, networks, limit, lang, json),
    }
}

async fn serve(common: &Common, data_dir: &Path, args: ServeArgs) -> Result<()> {
    let daemon = omnid::start(DaemonConfig {
        data_dir: data_dir.to_owned(),
        listen: args.listen,
        gateway: gateway_config(common, data_dir),
        crawl: crawl_config(&args.crawl),
        safety: safety(&args.crawl)?,
        seeds: seeds(&args.crawl)?,
        api_token: args.api_token,
        start_paused: args.paused,
        socks: args.socks,
        browser_proxy: None,
        dht: (!common.no_dht).then(DhtConfig::default),
        dht_harvest: args.crawl.dht_harvest,
    })
    .await?;
    let stop = daemon.shutdown_token();
    tokio::spawn(async move {
        terminated().await;
        stop.cancel();
    });
    let stats = daemon.wait().await?;
    tracing::info!(?stats, "stopped");
    Ok(())
}

async fn crawl(common: &Common, data_dir: &Path, args: CrawlArgs) -> Result<()> {
    let gateway =
        Arc::new(Gateway::new(gateway_config(common, data_dir)).map_err(|e| anyhow!("starting the gateway: {e}"))?);
    let index = Arc::new(omnid::open_index(data_dir)?);
    let store = Arc::new(omnid::open_store(data_dir)?);
    let mut seeds = seeds(&args.crawl)?;
    for address in &args.addresses {
        let source = Source::parse(address).with_context(|| format!("seed `{address}`"))?;
        seeds.push(Job { source, depth: 0, trust: SEED_TRUST });
    }
    let mut crawler = Crawler::new(gateway, index, Arc::new(safety(&args.crawl)?), store, crawl_config(&args.crawl));
    if !common.no_dht {
        crawler = crawler.with_dht(Dht::start(DhtConfig::default()).await.context("joining the DHT")?);
    }

    // With no seeds this resumes whatever the store has due.
    let (jobs, rx) = mpsc::channel(seeds.len().max(1));
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
    let control = crawler.control();
    let progress = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.tick().await;
        loop {
            tick.tick().await;
            tracing::info!(stats = ?control.stats(), "crawling");
        }
    });
    let stats = crawler.run(rx, shutdown).await;
    progress.abort();
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

async fn fetch(common: &Common, data_dir: &Path, address: &str) -> Result<()> {
    let source = Source::parse(address)?;
    if let Source::Magnet(magnet) = &source {
        anyhow::ensure!(!common.no_dht, "magnet metadata comes from the DHT, which --no-dht turns off");
        let dht = Dht::start(DhtConfig::default()).await.context("joining the DHT")?;
        let meta = dht.metadata(magnet.info_hash).await?;
        println!("network       magnet");
        println!("address       {source}");
        println!("name          {}", meta.name);
        println!("peers         {} found", meta.peers);
        println!("size          {} bytes in {} files", meta.total_size, meta.files.len());
        for file in meta.files.iter().take(20) {
            println!("  {:>14}  {}", file.size, file.path);
        }
        if meta.files.len() > 20 {
            println!("  ... and {} more", meta.files.len() - 20);
        }
        return Ok(());
    }
    let gateway = Gateway::new(gateway_config(common, data_dir)).map_err(|e| anyhow!("starting the gateway: {e}"))?;
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

fn search(
    data_dir: &Path,
    text: String,
    networks: Vec<Network>,
    limit: usize,
    lang: Vec<String>,
    json: bool,
) -> Result<()> {
    let dir = omnid::index_dir(data_dir);
    let index = SearchIndex::open_read_only(&dir)
        .with_context(|| format!("opening {}; run `omnid crawl` or `omnid serve` first", dir.display()))?;
    // LANG=de_DE.UTF-8 means German; C and POSIX mean no preference.
    let languages = if lang.is_empty() {
        std::env::var("LANG").ok().and_then(|l| omnid_core::lang::primary_subtag(&l)).into_iter().collect()
    } else {
        lang.iter().filter_map(|l| omnid_core::lang::primary_subtag(l)).collect()
    };
    let results = index.search(&SearchQuery { text, networks, limit, languages, ..SearchQuery::default() })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }
    if results.hits.is_empty() {
        println!("no results");
        return Ok(());
    }
    let counts: Vec<_> =
        results.network_counts.iter().filter(|(_, n)| **n > 0).map(|(net, n)| format!("{net} {n}")).collect();
    println!(
        "{} results ({}){}\n",
        results.total,
        counts.join(", "),
        if results.relaxed { ", matching any word" } else { "" }
    );
    for hit in results.hits {
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

fn gateway_config(common: &Common, data_dir: &Path) -> GatewayConfig {
    GatewayConfig {
        tor_dir: (!common.no_tor).then(|| data_dir.join("tor")),
        i2p_proxy: Some(common.i2p_proxy.clone()),
        fproxy: Some(common.fproxy.clone()),
        allow_private_addresses: common.allow_private_addresses,
        clearnet_via_tor: common.clearnet_via_tor,
        ..GatewayConfig::default()
    }
}

fn crawl_config(opts: &CrawlOptions) -> CrawlConfig {
    CrawlConfig {
        max_depth: opts.max_depth,
        max_pages: opts.max_pages,
        clearnet_same_site_only: !opts.follow_offsite,
        ..CrawlConfig::default()
    }
}

fn safety(opts: &CrawlOptions) -> Result<Safety> {
    Safety::load(opts.denylist.as_deref(), opts.blocked_terms.as_deref()).context("loading safety lists")
}

fn seeds(opts: &CrawlOptions) -> Result<Vec<Job>> {
    opts.seeds.as_deref().map(omnid::read_seeds).transpose().map(Option::unwrap_or_default)
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

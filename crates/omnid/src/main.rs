//! omnid: one daemon for Tor, I2P, Hyphanet, magnets and the clearnet, with
//! one search index over all of them.

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use omnid::DaemonConfig;
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, Crawler, Job, SEED_TRUST, Safety, extract, is_indexable_type};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SearchIndex, SearchQuery};
use omnid_net::{DownloadOptions, FetchOptions, Gateway, GatewayConfig, Progress};
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
    /// i2pd's HTTP proxy. Repeat it (or separate with commas) for more
    /// routers, best first: one that cannot be reached is passed over
    #[arg(long, env = "OMNID_I2P_PROXY", value_delimiter = ',', default_value = omnid_net::DEFAULT_I2P_PROXY, global = true)]
    i2p_proxy: Vec<Url>,
    /// I2P routers' SAM bridges (sam://host:port), best first: through one,
    /// each site the app's browser is on and each user of the password proxy
    /// gets an I2P identity of its own [default: port 7656 on each
    /// --i2p-proxy's host]
    #[arg(long, env = "OMNID_I2P_SAM", value_delimiter = ',', global = true)]
    i2p_sam: Vec<Url>,
    /// Reach I2P through the HTTP proxies alone, where every site shares their
    /// one destination
    #[arg(long, global = true, conflicts_with = "i2p_sam")]
    no_i2p_sam: bool,
    /// A Hyphanet node's FProxy. Repeat it (or separate with commas) for more
    /// nodes, best first
    #[arg(long, env = "OMNID_FPROXY", value_delimiter = ',', default_value = omnid_net::DEFAULT_FPROXY, global = true)]
    fproxy: Vec<Url>,
    /// An IPFS gateway that serves /ipfs/ and /ipns/ paths, such as an IPFS
    /// node's own (kubo's). Repeat it (or separate with commas) for more,
    /// best first
    #[arg(long, env = "OMNID_IPFS_GATEWAY", value_delimiter = ',', default_value = omnid_net::DEFAULT_IPFS_GATEWAY, global = true)]
    ipfs_gateway: Vec<Url>,
    /// Let clearnet fetches reach loopback and private networks
    #[arg(long, global = true)]
    allow_private_addresses: bool,
    /// Reach the clearnet through Tor exits instead of directly
    #[arg(long, global = true)]
    clearnet_via_tor: bool,
    /// Do not join the BitTorrent DHT (magnets are then indexed from their links alone)
    #[arg(long, global = true)]
    no_dht: bool,
    /// Nostr relays (wss://), asked four at a time. Repeat it (or separate
    /// with commas) for more. They are reached as the clearnet is: directly,
    /// or through Tor exits with --clearnet-via-tor
    #[arg(long, env = "OMNID_NOSTR_RELAY", value_delimiter = ',', default_values = omnid_net::DEFAULT_NOSTR_RELAYS, global = true)]
    nostr_relay: Vec<Url>,
    /// Do not read Nostr relays
    #[arg(long, global = true, conflicts_with = "nostr_relay")]
    no_nostr: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: routers, crawler and the local API
    Serve(ServeArgs),
    /// Crawl from seeds until there is nothing left to crawl, then exit
    Crawl(CrawlArgs),
    /// Fetch one address through the gateway and describe the response
    Fetch {
        /// A URL, bare host, freenet: key, ipfs:// or ipns:// address, nostr: entity, or magnet link
        address: String,
    },
    /// Download a file through the gateway: in byte ranges over several
    /// connections when the server sends them, over Tor each on circuits of
    /// its own
    Download(DownloadArgs),
    /// Search the local index
    Search {
        query: String,
        /// Only these networks (repeatable): clearnet, onion, i2p, hyphanet, ipfs, nostr, magnet
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
    /// Serve a proxy here (e.g. 127.0.0.1:7172), SOCKS5 and HTTP on one port,
    /// so browsers and other apps open .onion and .i2p addresses through
    /// omnid; with SOCKS use remote DNS (socks5h)
    #[arg(long, env = "OMNID_SOCKS")]
    socks: Option<SocketAddr>,
    /// Make the proxy private: only clients that send this password get in
    /// (as SOCKS5 or HTTP Basic credentials, any username), nothing goes
    /// direct (the clearnet through Tor exits), and each username gets Tor
    /// circuits of its own
    #[arg(long, env = "OMNID_SOCKS_PASSWORD", hide_env_values = true, requires = "socks")]
    socks_password: Option<String>,
    #[command(flatten)]
    crawl: CrawlOptions,
}

#[derive(Args)]
struct DownloadArgs {
    /// A URL, bare host or freenet: key
    address: String,
    /// Where to save it: a file, or a directory to put it in under the name
    /// the server gives [default: that name, here]
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Connections at once
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=omnid_net::MAX_CONNECTIONS as i64))]
    connections: u8,
    /// Refuse a file larger than this many bytes
    #[arg(long, default_value_t = DownloadOptions::default().max_bytes)]
    max_bytes: u64,
    /// Replace a file that is already there
    #[arg(long)]
    force: bool,
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
    /// Sites to follow from a seed: a link to another site is a hop, links
    /// within a site are not
    #[arg(long, default_value_t = 5)]
    max_depth: u32,
    /// Links to follow within one site, from the page the crawl entered it by
    #[arg(long, default_value_t = 4)]
    max_site_depth: u32,
    /// Addresses to keep per site, so one big site cannot fill the crawl
    #[arg(long, default_value_t = 500)]
    max_site_pages: usize,
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
    /// Leave out Ahmia's list of onion services reported for abuse, which is
    /// otherwise kept in the safety gate and refreshed daily over Tor
    #[arg(long)]
    no_blocklist: bool,
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
        Command::Download(args) => download(&cli.common, &data_dir, args).await,
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
        seeds: seeds_or_defaults(seeds(&args.crawl)?, data_dir)?,
        api_token: args.api_token,
        start_paused: args.paused,
        socks: args.socks.map(|addr| {
            let access = match args.socks_password {
                Some(password) => omnid::ProxyAccess::Apps { password: Some(password) },
                None => omnid::ProxyAccess::Open,
            };
            (addr, access)
        }),
        browser_proxy: None,
        dht: (!common.no_dht).then(DhtConfig::default),
        dht_harvest: args.crawl.dht_harvest,
        abuse_blocklist: !args.crawl.no_blocklist,
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
        seeds.push(Job { source, depth: 0, site_depth: 0, trust: SEED_TRUST });
    }
    let seeds = seeds_or_defaults(seeds, data_dir)?;
    let safety = Arc::new(safety(&args.crawl)?);
    if !args.crawl.no_blocklist {
        omnid::load_blocklist(&safety, data_dir);
        match omnid::refresh_blocklist(&gateway, &safety, data_dir).await {
            Ok(sites) => tracing::info!(sites, "abuse blocklist refreshed from Ahmia"),
            Err(e) => tracing::warn!("abuse blocklist: {e:#}; using the last copy, if any"),
        }
    }
    let mut crawler = Crawler::new(gateway, index, safety, store, crawl_config(&args.crawl));
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
    if let Source::Nostr(entity) = &source {
        let events = gateway.nostr(&omnid_crawl::nostr::filter(&entity.target), &entity.relays).await?;
        println!("network       nostr");
        println!("address       {source}");
        println!("events        {} (each one's ID and signature checked)", events.len());
        for event in events.iter().take(20) {
            let first = event.content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
            let first: String = first.chars().take(100).collect();
            println!(
                "  kind {:<5} {}  {}  {first}",
                event.kind,
                omnid_core::nostr::npub(&event.pubkey),
                event.created_at
            );
        }
        return Ok(());
    }
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

async fn download(common: &Common, data_dir: &Path, args: DownloadArgs) -> Result<()> {
    let url = match Source::parse(&args.address)? {
        Source::Hyphanet(key) => Url::parse(&format!("http://{}/{key}", omnid_net::HYPHANET_HOST))?,
        Source::Magnet(_) => anyhow::bail!("a magnet is not a file; `omnid fetch` shows its metadata"),
        Source::Ipfs(_) => anyhow::bail!("downloading from IPFS is not built yet; `ipfs get` on your node fetches it"),
        Source::Nostr(_) => anyhow::bail!("a Nostr event is not a file; `omnid fetch` shows it"),
        source => source.url().cloned().context("no URL to download")?,
    };
    // A file named and already there is refused before anything is fetched.
    let named = args.output.clone().filter(|path| !path.is_dir());
    if let Some(path) = &named {
        refuse_existing(path, args.force)?;
    }
    let dir = match (&args.output, &named) {
        (Some(dir), None) => dir.clone(),
        (_, Some(path)) => path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_owned(),
        (None, None) => PathBuf::from("."),
    };
    // Into a hidden file beside the target, renamed once it is complete.
    let partial = dir.join(format!(".omnid-download-{}.part", std::process::id()));
    let file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&partial)
        .with_context(|| format!("creating {}", partial.display()))?;
    let saved = async {
        let gateway =
            Gateway::new(gateway_config(common, data_dir)).map_err(|e| anyhow!("starting the gateway: {e}"))?;
        let opts = DownloadOptions {
            connections: args.connections.into(),
            max_bytes: args.max_bytes,
            ..DownloadOptions::default()
        };
        let (progress, cancel) = (Arc::new(Progress::default()), CancellationToken::new());
        let stop = cancel.clone();
        tokio::spawn(async move {
            terminated().await;
            stop.cancel();
        });
        let started = Instant::now();
        let shown = progress.clone();
        let terminal = std::io::stderr().is_terminal();
        let ticker = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                if terminal {
                    eprint!("\r\x1b[K{}", progress_line(&shown, started.elapsed()));
                }
            }
        });
        let downloaded = gateway.download(&url, &file, &opts, &progress, &cancel).await;
        ticker.abort();
        if terminal {
            eprint!("\r\x1b[K");
        }
        let downloaded = downloaded?;
        file.sync_all()?;
        let target = named.clone().unwrap_or_else(|| dir.join(&downloaded.filename));
        refuse_existing(&target, args.force)?;
        std::fs::rename(&partial, &target).with_context(|| format!("saving as {}", target.display()))?;
        anyhow::Ok((downloaded, target, started.elapsed()))
    };
    let (downloaded, target, elapsed) = match saved.await {
        Ok(saved) => saved,
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
    };
    let seconds = elapsed.as_secs_f64();
    println!("saved         {}", target.display());
    println!("address       {}", downloaded.url);
    println!("content-type  {}", downloaded.content_type.as_deref().unwrap_or("-"));
    println!("bytes         {}", downloaded.bytes);
    println!("seconds       {seconds:.1}");
    println!("MB/s          {:.2}", downloaded.bytes as f64 / 1e6 / seconds.max(1e-3));
    let ranges = if downloaded.ranged { "" } else { " (the server sent no ranges)" };
    println!("connections   {}{ranges}", downloaded.connections);
    println!("retries       {}", downloaded.retries);
    Ok(())
}

/// `12.3 / 40.0 MB  1.23 MB/s  4 connections  0 retries`
fn progress_line(progress: &Progress, elapsed: Duration) -> String {
    let mb = |bytes: u64| bytes as f64 / 1e6;
    let total = match progress.total() {
        0 => String::new(),
        total => format!(" / {:.1}", mb(total)),
    };
    format!(
        "{:.1}{total} MB  {:.2} MB/s  {} connections  {} retries",
        mb(progress.done()),
        mb(progress.done()) / elapsed.as_secs_f64().max(1e-3),
        progress.active(),
        progress.retries()
    )
}

/// A dangling symlink counts: renaming onto it would replace it.
fn refuse_existing(path: &Path, force: bool) -> Result<()> {
    anyhow::ensure!(
        force || std::fs::symlink_metadata(path).is_err(),
        "{} exists; --force replaces it",
        path.display()
    );
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
        i2p_proxies: common.i2p_proxy.clone(),
        i2p_sam: match (common.no_i2p_sam, common.i2p_sam.is_empty()) {
            (true, _) => Vec::new(),
            (false, true) => omnid_net::sam_beside(&common.i2p_proxy),
            (false, false) => common.i2p_sam.clone(),
        },
        fproxies: common.fproxy.clone(),
        ipfs_gateways: common.ipfs_gateway.clone(),
        nostr_relays: if common.no_nostr { Vec::new() } else { common.nostr_relay.clone() },
        allow_private_addresses: common.allow_private_addresses,
        clearnet_via_tor: common.clearnet_via_tor,
        ..GatewayConfig::default()
    }
}

fn crawl_config(opts: &CrawlOptions) -> CrawlConfig {
    CrawlConfig {
        max_depth: opts.max_depth,
        max_site_depth: opts.max_site_depth,
        max_site_pages: opts.max_site_pages,
        max_pages: opts.max_pages,
        clearnet_same_site_only: !opts.follow_offsite,
        ..CrawlConfig::default()
    }
}

fn safety(opts: &CrawlOptions) -> Result<Safety> {
    Safety::load(opts.denylist.as_deref(), opts.blocked_terms.as_deref()).context("loading safety lists")
}

/// The starter seeds for an empty store given none of its own.
fn seeds_or_defaults(seeds: Vec<Job>, data_dir: &Path) -> Result<Vec<Job>> {
    if !seeds.is_empty() || omnid::open_store(data_dir)?.known()? > 0 {
        return Ok(seeds);
    }
    let defaults = omnid::default_seeds();
    tracing::info!(seeds = defaults.len(), "a new index: starting from the default seeds");
    Ok(defaults)
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

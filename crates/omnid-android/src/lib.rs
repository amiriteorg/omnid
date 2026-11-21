//! Runs the omnid daemon inside the Android app.
//!
//! The Kotlin side (`cc.wckd.omnid.Native`) starts it from a foreground
//! service, points a WebView at the port it returns, and pauses crawling
//! when the phone is off its charger or off an unmetered network. The daemon
//! is the same one `omnid serve` runs; only the defaults are phone-sized.

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

use anyhow::{Context, Result};
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JLongArray, JString};
use jni::sys::{jboolean, jint, jlong};
use omnid::{Daemon, DaemonConfig};
use omnid_core::{Network, now_secs};
use omnid_crawl::{CrawlConfig, Politeness, Safety};
use omnid_net::{DownloadError, DownloadOptions, Expected, GatewayConfig, Progress, Route};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
/// Downloads the app's browser handed over (`Native.download`), by id, until
/// the app forgets them.
static DOWNLOADS: Mutex<BTreeMap<i64, Arc<DownloadJob>>> = Mutex::new(BTreeMap::new());
static NEXT_DOWNLOAD: AtomicI64 = AtomicI64::new(1);
static LOGGING: Once = Once::new();

struct Running {
    runtime: tokio::runtime::Runtime,
    daemon: Daemon,
}

/// Starts the daemon if it is not running and returns its port. Crawling
/// starts paused; the caller applies its battery and network policy with
/// [`set_paused`].
pub fn start(data_dir: &Path, token: &str, options: &Options) -> Result<u16> {
    start_with(data_dir, token, options, true)
}

/// The app's settings the daemon takes at start, as `Native.start` passes
/// them: a JSON object with these fields, each optional.
#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    /// A Hyphanet node's FProxy elsewhere (the owner's computer over
    /// Tailscale, say), or empty for only one on this phone.
    pub hyphanet_node: String,
    /// An i2pd HTTP proxy elsewhere, or empty for only the i2pd app's.
    pub i2p_router: String,
    /// The ordinal of the app's `Settings.CrawlSpeed`.
    pub crawl_speed: i32,
    /// The proxy for other apps on the phone, when it is on.
    pub app_proxy: Option<AppProxy>,
    /// Ahmia's blocklist of onion services in the safety gate; on unless the
    /// app's Settings turn it off.
    pub abuse_blocklist: bool,
    /// The crawler reaches the clearnet through Tor exits, on unless the app's
    /// Settings turn it off: directly, a page found on an onion site could
    /// make the phone fetch a clearnet address its author watches, from the
    /// phone's own IP.
    pub clearnet_via_tor: bool,
    /// Joins the BitTorrent DHT for magnets' names and file lists; off unless
    /// the app's Settings turn it on. It cannot go through Tor: the DHT and
    /// the peers asked for metadata see the phone's IP and every torrent it
    /// looks up, those found on onion sites included.
    pub torrent_lookups: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hyphanet_node: String::new(),
            i2p_router: String::new(),
            crawl_speed: 1,
            app_proxy: None,
            abuse_blocklist: true,
            clearnet_via_tor: true,
            torrent_lookups: false,
        }
    }
}

/// Settings → Proxy for other apps: SOCKS5 and HTTP on this loopback port,
/// never direct, behind the password if there is one.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProxy {
    pub port: u16,
    pub password: Option<String>,
}

/// How hard the phone crawls when its policy lets it: the app's Settings →
/// Crawl speed, as the ordinal of its `Settings.CrawlSpeed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrawlSpeed {
    /// Half the connections per network, for a phone that should stay cool.
    Gentle,
    #[default]
    Normal,
    /// Twice the connections per network; sites still get one request at a
    /// time with their usual pause.
    Fast,
}

impl CrawlSpeed {
    pub fn from_ordinal(ordinal: i32) -> Self {
        match ordinal {
            0 => Self::Gentle,
            2 => Self::Fast,
            _ => Self::Normal,
        }
    }

    fn scale(self, concurrency: usize) -> usize {
        match self {
            Self::Gentle => concurrency.div_ceil(2),
            Self::Normal => concurrency,
            Self::Fast => concurrency * 2,
        }
    }
}

/// `networks: false` leaves out Tor and the DHT, for tests.
fn start_with(data_dir: &Path, token: &str, options: &Options, networks: bool) -> Result<u16> {
    let mut running = RUNNING.lock().expect("daemon lock poisoned");
    if let Some(running) = running.as_ref() {
        return Ok(running.daemon.addr().port());
    }
    LOGGING.call_once(init_logging);
    let speed = CrawlSpeed::from_ordinal(options.crawl_speed);
    tracing::info!(
        ?speed,
        app_proxy = options.app_proxy.is_some(),
        clearnet_via_tor = options.clearnet_via_tor,
        torrent_lookups = options.torrent_lookups,
        "starting the daemon"
    );

    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    // The starter seeds a new install needs and an older one is missing:
    // added once, due now; the ones already known are left alone.
    let added = omnid::open_store(data_dir)?.discover(&omnid::default_seeds(), now_secs())?;
    if added > 0 {
        tracing::info!(added, "starter seeds added");
    }
    // Operators can drop lists into the app's data directory.
    let list = |name: &str| Some(data_dir.join(name)).filter(|p| p.exists());
    let safety = Safety::load(list("denylist.txt").as_deref(), list("blocked-terms.txt").as_deref())?;
    let gateway = gateway(data_dir, options, networks)?;
    let app_proxy = options.app_proxy.as_ref().and_then(|proxy| {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, proxy.port));
        // Another app may hold the port: the daemon starts without the proxy
        // rather than not at all, and the app shows it is not running.
        if let Err(e) = std::net::TcpListener::bind(addr) {
            tracing::warn!("the proxy for other apps cannot have {addr}: {e}");
            return None;
        }
        Some((addr, omnid::ProxyAccess::Apps { password: proxy.password.clone().filter(|p| !p.is_empty()) }))
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("omnid")
        .enable_all()
        .build()
        .context("starting the runtime")?;
    let daemon = runtime.block_on(omnid::start(DaemonConfig {
        data_dir: data_dir.to_owned(),
        listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        gateway,
        crawl: phone_crawl(speed),
        safety,
        seeds: vec![],
        api_token: Some(token.to_owned()),
        start_paused: true,
        socks: app_proxy,
        // The app's browser, behind the same secret as the API.
        browser_proxy: Some(token.to_owned()),
        // Magnets get their file lists; harvesting the whole DHT is a
        // server's job, not a phone's.
        dht: (networks && options.torrent_lookups)
            .then(|| omnid_dht::DhtConfig { max_peer_connections: 4, ..omnid_dht::DhtConfig::default() }),
        dht_harvest: false,
        abuse_blocklist: networks && options.abuse_blocklist,
    }))?;
    let port = daemon.addr().port();
    *running = Some(Running { runtime, daemon });
    Ok(port)
}

/// The phone's gateway: Tor in the app's data directory, and the routers it
/// was given ahead of the phone's own: the i2pd app's proxy and a node's
/// FProxy on this phone, where they listen by default, stay as the fallback.
fn gateway(data_dir: &Path, options: &Options, networks: bool) -> Result<GatewayConfig> {
    let mut gateway = GatewayConfig {
        tor_dir: networks.then(|| data_dir.join("tor")),
        clearnet_via_tor: networks && options.clearnet_via_tor,
        ..GatewayConfig::default()
    };
    if let Some(node) = elsewhere(&options.hyphanet_node, "Hyphanet node", &["http", "https"])? {
        gateway.fproxies.retain(|n| *n != node);
        gateway.fproxies.insert(0, node);
    }
    // i2pd's proxy speaks plain HTTP only.
    if let Some(router) = elsewhere(&options.i2p_router, "I2P router", &["http"])? {
        gateway.i2p_proxies.retain(|r| *r != router);
        gateway.i2p_proxies.insert(0, router);
    }
    Ok(gateway)
}

/// A router's address as the app passes it, or `None` for an empty one.
fn elsewhere(address: &str, what: &str, schemes: &[&str]) -> Result<Option<url::Url>> {
    let address = address.trim();
    if address.is_empty() {
        return Ok(None);
    }
    let url: url::Url = address.parse().with_context(|| format!("{what} {address:?}"))?;
    anyhow::ensure!(
        schemes.contains(&url.scheme()) && url.host().is_some(),
        "{what} {address:?} is not an {} address",
        schemes.join(" or ")
    );
    Ok(Some(url))
}

/// A download in the daemon, as the app polls it.
struct DownloadJob {
    progress: Arc<Progress>,
    cancel: CancellationToken,
    /// `None` while it runs.
    outcome: Mutex<Option<Outcome>>,
}

enum Outcome {
    Done,
    /// Not the file the browser saw: the app falls back to the browser's copy.
    Changed(String),
    Failed(String),
    Cancelled,
}

/// What the app reads of a download: `Native.downloadState`'s last value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum DownloadState {
    /// Asking the server; the browser's own copy is still held.
    Probing = 0,
    /// The server's answer is the file the browser saw, and it is coming.
    Running = 1,
    Done = 2,
    Failed = 3,
    Changed = 4,
    Cancelled = 5,
}

/// What the browser saw of the file, to check the daemon's answer against.
#[derive(Debug, Default)]
pub struct Seen {
    pub length: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Starts downloading `url` into `file` in the running daemon, over the
/// browser's route for `site` (never direct), in up to `connections`
/// parallel ranges each on Tor circuits of its own. Returns its id.
pub fn download(
    url: &str,
    site: &str,
    user_agent: &str,
    file: std::fs::File,
    connections: usize,
    seen: Seen,
) -> Result<i64> {
    let running = RUNNING.lock().expect("daemon lock poisoned");
    let running = running.as_ref().context("OMNId is not running")?;
    let url: url::Url = url.parse().context("not an address")?;
    let job =
        Arc::new(DownloadJob { progress: Arc::default(), cancel: CancellationToken::new(), outcome: Mutex::new(None) });
    let id = NEXT_DOWNLOAD.fetch_add(1, Ordering::Relaxed);
    DOWNLOADS.lock().expect("downloads lock poisoned").insert(id, job.clone());
    let opts = DownloadOptions {
        connections: connections.clamp(1, omnid_net::MAX_CONNECTIONS),
        user_agent: Some(user_agent.to_owned()).filter(|agent| !agent.is_empty()),
        route: Route::Browser { site: site.to_owned() },
        expect: Some(Expected { length: seen.length, etag: seen.etag, last_modified: seen.last_modified }),
        ..DownloadOptions::default()
    };
    let gateway = running.daemon.gateway().clone();
    running.runtime.spawn(async move {
        let result = gateway.download(&url, &file, &opts, &job.progress, &job.cancel).await;
        // The address stays out of the log: it is the user's.
        let outcome = match result {
            Ok(done) => {
                tracing::info!(
                    bytes = done.bytes,
                    ranged = done.ranged,
                    connections = done.connections,
                    "download finished"
                );
                Outcome::Done
            }
            Err(DownloadError::Cancelled) => Outcome::Cancelled,
            Err(e @ (DownloadError::Mismatch(_) | DownloadError::Changed)) => Outcome::Changed(e.to_string()),
            Err(e) => {
                tracing::info!("download failed: {e}");
                Outcome::Failed(e.to_string())
            }
        };
        job.outcome.lock().expect("download lock poisoned").get_or_insert(outcome);
    });
    Ok(id)
}

/// Bytes done, the size (-1 while unknown), connections open, and the
/// [`DownloadState`].
pub fn download_state(id: i64) -> [i64; 4] {
    let Some(job) = DOWNLOADS.lock().expect("downloads lock poisoned").get(&id).cloned() else {
        return [0, -1, 0, DownloadState::Failed as i64];
    };
    let progress = &job.progress;
    let state = match &*job.outcome.lock().expect("download lock poisoned") {
        // The engine counts a size or bytes only once the answer matched.
        None if progress.total() > 0 || progress.done() > 0 => DownloadState::Running,
        None => DownloadState::Probing,
        Some(Outcome::Done) => DownloadState::Done,
        Some(Outcome::Changed(_)) => DownloadState::Changed,
        Some(Outcome::Failed(_)) => DownloadState::Failed,
        Some(Outcome::Cancelled) => DownloadState::Cancelled,
    };
    let total = if progress.total() > 0 { progress.total() as i64 } else { -1 };
    [progress.done() as i64, total, progress.active() as i64, state as i64]
}

/// Why a download failed or changed; empty otherwise.
pub fn download_error(id: i64) -> String {
    let job = DOWNLOADS.lock().expect("downloads lock poisoned").get(&id).cloned();
    let Some(job) = job else { return "no such download".into() };
    match &*job.outcome.lock().expect("download lock poisoned") {
        Some(Outcome::Changed(why) | Outcome::Failed(why)) => why.clone(),
        _ => String::new(),
    }
}

pub fn cancel_download(id: i64) {
    if let Some(job) = DOWNLOADS.lock().expect("downloads lock poisoned").get(&id) {
        job.cancel.cancel();
    }
}

pub fn forget_download(id: i64) {
    DOWNLOADS.lock().expect("downloads lock poisoned").remove(&id);
}

/// Stops crawling, commits the index and shuts the daemon down.
pub fn stop() {
    let Some(Running { runtime, daemon }) = RUNNING.lock().expect("daemon lock poisoned").take() else {
        return;
    };
    daemon.shutdown();
    // Downloads die with the runtime: say so, so the app does not wait on them.
    for job in DOWNLOADS.lock().expect("downloads lock poisoned").values() {
        job.cancel.cancel();
        job.outcome.lock().expect("download lock poisoned").get_or_insert(Outcome::Failed("OMNId stopped".into()));
    }
    // The timer is made inside the runtime: the caller's thread has none.
    match runtime.block_on(async { tokio::time::timeout(Duration::from_secs(15), daemon.wait()).await }) {
        Ok(Ok(stats)) => tracing::info!(?stats, "stopped"),
        Ok(Err(e)) => tracing::error!("stopping: {e:#}"),
        Err(_) => tracing::warn!("timed out waiting for the crawl to stop"),
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
}

/// The port of the browser's SOCKS5 listener, which wants the API token as
/// its password; 0 when the daemon is not running.
pub fn browser_proxy_port() -> u16 {
    let running = RUNNING.lock().expect("daemon lock poisoned");
    running.as_ref().and_then(|r| r.daemon.browser_proxy_addr()).map_or(0, |a| a.port())
}

/// The port of the proxy for other apps, 0 when it is off or could not
/// have its port.
pub fn app_proxy_port() -> u16 {
    let running = RUNNING.lock().expect("daemon lock poisoned");
    running.as_ref().and_then(|r| r.daemon.socks_addr()).map_or(0, |a| a.port())
}

/// New Tor circuits for every site the browser visits from now on.
pub fn new_identity() {
    if let Some(running) = RUNNING.lock().expect("daemon lock poisoned").as_ref() {
        running.daemon.new_identity();
    }
}

/// Pauses or resumes crawling. A non-empty `reason` is shown in the UI.
pub fn set_paused(paused: bool, reason: &str) {
    if let Some(running) = RUNNING.lock().expect("daemon lock poisoned").as_ref() {
        let control = running.daemon.control();
        if paused && !reason.is_empty() { control.pause_because(reason) } else { control.set_paused(paused) }
    }
}

/// Fewer connections, shallower crawls and smaller pages than a desktop:
/// a phone crawls in the background while it charges, not as a server. It
/// still goes wide: four sites from a seed, a few pages deep into each, and
/// no more than 150 addresses from any one site. `speed` scales the
/// connections per network, never the pause each site gets.
fn phone_crawl(speed: CrawlSpeed) -> CrawlConfig {
    let politeness = |concurrency, delay_secs| Politeness {
        concurrency: speed.scale(concurrency),
        site_delay: Duration::from_secs(delay_secs),
    };
    CrawlConfig {
        max_depth: 4,
        max_site_depth: 3,
        max_site_pages: 150,
        max_page_bytes: 1 << 20,
        frontier_capacity: 5_000,
        commit_interval: Duration::from_secs(30),
        politeness: HashMap::from([
            (Network::Clearnet, politeness(6, 1)),
            (Network::Onion, politeness(8, 5)),
            (Network::I2p, politeness(3, 5)),
            (Network::Hyphanet, politeness(2, 2)),
            (Network::Magnet, politeness(4, 0)),
        ]),
        ..CrawlConfig::default()
    }
}

#[derive(Debug, thiserror::Error)]
enum NativeError {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
    #[error("{0}")]
    Daemon(String),
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_start<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
    token: JString<'local>,
    options: JString<'local>,
) -> jint {
    env.with_env(|_| -> Result<jint, NativeError> {
        let options: Options = serde_json::from_str(&options.to_string())
            .map_err(|e| NativeError::Daemon(format!("the app's options: {e}")))?;
        let port = start(Path::new(&data_dir.to_string()), &token.to_string(), &options)
            .map_err(|e| NativeError::Daemon(format!("{e:#}")))?;
        Ok(jint::from(port))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_stop<'local>(mut env: EnvUnowned<'local>, _class: JClass<'local>) {
    env.with_env(|_| -> Result<(), NativeError> {
        stop();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_setPaused<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    paused: jboolean,
    reason: JString<'local>,
) {
    env.with_env(|_| -> Result<(), NativeError> {
        set_paused(paused, &reason.to_string());
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_browserProxyPort<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jint {
    env.with_env(|_| -> Result<jint, NativeError> { Ok(jint::from(browser_proxy_port())) })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_appProxyPort<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jint {
    env.with_env(|_| -> Result<jint, NativeError> { Ok(jint::from(app_proxy_port())) })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_cc_wckd_omnid_Native_download<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    url: JString<'local>,
    site: JString<'local>,
    user_agent: JString<'local>,
    fd: jint,
    connections: jint,
    length: jlong,
    etag: JString<'local>,
    last_modified: JString<'local>,
) -> jlong {
    env.with_env(|_| -> Result<jlong, NativeError> {
        // SAFETY: the app detached this descriptor from its ParcelFileDescriptor
        // for us: nothing else owns or closes it.
        let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let text = |s: &JString| Some(s.to_string()).filter(|s| !s.is_empty());
        let seen = Seen { length: u64::try_from(length).ok(), etag: text(&etag), last_modified: text(&last_modified) };
        let connections = usize::try_from(connections).unwrap_or(1);
        download(&url.to_string(), &site.to_string(), &user_agent.to_string(), file, connections, seen)
            .map_err(|e| NativeError::Daemon(format!("{e:#}")))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_downloadState<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    id: jlong,
) -> JLongArray<'local> {
    env.with_env(|env| -> Result<JLongArray<'local>, NativeError> {
        let array = env.new_long_array(4)?;
        array.set_region(env, 0, &download_state(id))?;
        Ok(array)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_downloadError<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    id: jlong,
) -> JString<'local> {
    env.with_env(|env| -> Result<JString<'local>, NativeError> { Ok(env.new_string(download_error(id))?) })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_cancelDownload<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    id: jlong,
) {
    env.with_env(|_| -> Result<(), NativeError> {
        cancel_download(id);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_forgetDownload<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    id: jlong,
) {
    env.with_env(|_| -> Result<(), NativeError> {
        forget_download(id);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_cc_wckd_omnid_Native_newIdentity<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    env.with_env(|_| -> Result<(), NativeError> {
        new_identity();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::new("info,tantivy=warn,tor_dirmgr=warn,tor_guardmgr=warn");
    #[cfg(target_os = "android")]
    let builder = tracing_subscriber::fmt().with_writer(|| logcat::Logcat).with_ansi(false).without_time();
    #[cfg(not(target_os = "android"))]
    let builder = tracing_subscriber::fmt();
    let _already_set = builder.with_env_filter(filter).try_init();
    // A panic's message and place otherwise go to stderr, which Android
    // drops; the app sees only "Rust panic: non-string panic payload".
    #[cfg(target_os = "android")]
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!("{info}");
            previous(info);
        }));
    }
}

/// tracing → logcat, under the tag `omnid`.
#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{CString, c_char, c_int};
    use std::io;

    const INFO: c_int = 4;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub struct Logcat;

    impl io::Write for Logcat {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let text = String::from_utf8_lossy(buf);
            let line = text.trim_end().replace('\0', " ");
            if let Ok(line) = CString::new(line) {
                // SAFETY: both pointers are valid NUL-terminated strings for the call.
                unsafe { __android_log_write(INFO, c"omnid".as_ptr(), line.as_ptr()) };
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crawl_speed_scales_connections_not_pauses() {
        let concurrency = |speed| phone_crawl(speed).politeness[&Network::Onion].concurrency;
        let speeds = [0, 1, 2].map(CrawlSpeed::from_ordinal);
        assert_eq!(speeds, [CrawlSpeed::Gentle, CrawlSpeed::Normal, CrawlSpeed::Fast], "the app's order");
        assert_eq!(speeds.map(concurrency), [4, 8, 16]);
        assert_eq!(phone_crawl(CrawlSpeed::Gentle).politeness[&Network::Hyphanet].concurrency, 1, "never none");
        let pause = |speed| phone_crawl(speed).politeness[&Network::Onion].site_delay;
        assert_eq!(pause(CrawlSpeed::Fast), pause(CrawlSpeed::Gentle));
        assert_eq!(CrawlSpeed::from_ordinal(7), CrawlSpeed::Normal, "unknown: normal");
    }

    fn tempfile_in(dir: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join("download"))
            .unwrap()
    }

    fn routers(hyphanet_node: &str, i2p_router: &str) -> Options {
        Options { hyphanet_node: hyphanet_node.into(), i2p_router: i2p_router.into(), ..Options::default() }
    }

    fn urls(urls: &[url::Url]) -> Vec<&str> {
        urls.iter().map(url::Url::as_str).collect()
    }

    #[test]
    fn takes_routers_elsewhere_ahead_of_the_phones_own() {
        let dir = Path::new("unused");
        let local = gateway(dir, &routers(" ", ""), false).unwrap();
        assert_eq!(urls(&local.fproxies), ["http://127.0.0.1:8888/"], "none given: a node on this phone");
        assert_eq!(urls(&local.i2p_proxies), ["http://127.0.0.1:4444/"], "and the i2pd app");

        let remote =
            gateway(dir, &routers("http://100.101.102.103:8888/", "http://100.101.102.103:4444/"), false).unwrap();
        assert_eq!(urls(&remote.fproxies), ["http://100.101.102.103:8888/", "http://127.0.0.1:8888/"]);
        assert_eq!(urls(&remote.i2p_proxies), ["http://100.101.102.103:4444/", "http://127.0.0.1:4444/"]);

        let same = gateway(dir, &routers("", "http://127.0.0.1:4444/"), false).unwrap();
        assert_eq!(urls(&same.i2p_proxies), ["http://127.0.0.1:4444/"], "the phone's own, once");

        for node in ["ftp://100.101.102.103/", "100.101.102.103:8888", "not a url"] {
            assert!(gateway(dir, &routers(node, ""), false).is_err(), "{node}");
        }
        for router in ["https://100.101.102.103:4444/", "100.101.102.103:4444"] {
            assert!(gateway(dir, &routers("", router), false).is_err(), "{router}");
        }
    }

    /// A restart (a new Hyphanet node, Retry) stops a daemon whose Tor and
    /// DHT are running; on the phone a panic there kills the app.
    #[test]
    #[ignore = "live: reaches Tor and the DHT"]
    fn live_stops_with_tor_and_the_dht_running() {
        let dir = std::env::temp_dir().join(format!("omnid-android-live-{}", std::process::id()));
        start_with(&dir, "token", &Options::default(), true).unwrap();
        std::thread::sleep(Duration::from_secs(20));
        stop();
        let again = start_with(&dir, "token", &Options::default(), true).unwrap();
        assert_ne!(again, 0, "and starts again after");
        stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// On a plain thread with no Tokio runtime around it, as the app's
    /// native-call thread is.
    #[test]
    fn starts_pauses_and_stops() {
        let dir = std::env::temp_dir().join(format!("omnid-android-test-{}", std::process::id()));
        // Without Tor or the DHT, so the test stays off the network. A node
        // elsewhere that does not answer: the status says where omnid looked.
        let options: Options = serde_json::from_str(
            r#"{"hyphanetNode": "http://127.0.0.1:9", "i2pRouter": "http://127.0.0.1:10", "crawlSpeed": 2,
                "appProxy": {"port": 0, "password": "pw"}}"#,
        )
        .unwrap();
        assert!(options.abuse_blocklist, "on unless the app says otherwise");
        assert!(options.clearnet_via_tor && !options.torrent_lookups, "anonymous unless the app says otherwise");
        let off: Options = serde_json::from_str(r#"{"abuseBlocklist": false}"#).unwrap();
        assert!(!off.abuse_blocklist);
        assert_eq!(off.crawl_speed, 1, "the rest as by default");
        let started = start_with(&dir, "token", &options, false).unwrap();
        assert_ne!(started, 0);
        let again = start_with(&dir, "token", &Options::default(), false).unwrap();
        assert_eq!(again, started, "a second start reuses the daemon");
        assert_ne!(browser_proxy_port(), 0, "the browser's proxy is up");
        // The proxy for other apps is up, and wants its password.
        let mut proxy = std::net::TcpStream::connect(("127.0.0.1", app_proxy_port())).unwrap();
        proxy.write_all(b"\x05\x01\x00").unwrap();
        let mut refused = [0u8; 2];
        proxy.read_exact(&mut refused).unwrap();
        assert_eq!(refused, [5, 0xff], "no authentication is not offered");
        new_identity();
        // A download goes the browser's way, never direct: with Tor off it
        // fails, though the server is right here, and says so.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/file", server.local_addr().unwrap().port());
        let file = tempfile_in(&dir);
        let id = download(&url, "127.0.0.1", "", file, 4, Seen::default()).unwrap();
        let state = (0..100)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(50));
                download_state(id)
            })
            .find(|state| state[3] != DownloadState::Probing as i64)
            .expect("the download ended");
        assert_eq!(state[3], DownloadState::Failed as i64, "{}", download_error(id));
        server.set_nonblocking(true).unwrap();
        assert!(server.accept().is_err(), "nothing reached the server");
        forget_download(id);
        assert_eq!(download_state(id)[3], DownloadState::Failed as i64, "a forgotten id is no download");
        set_paused(true, "waiting for a charger");
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", started)).unwrap();
        use std::io::{Read, Write};
        write!(stream, "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:{started}\r\nAuthorization: Bearer token\r\nConnection: close\r\n\r\n").unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        stop();
        assert_eq!(browser_proxy_port(), 0, "and down again");
        assert_eq!(app_proxy_port(), 0);
        assert!(body.starts_with("HTTP/1.1 200"), "{body}");
        assert!(body.contains("waiting for a charger"), "{body}");
        assert!(body.contains("no FProxy at http://127.0.0.1:9/"), "{body}");
        assert!(body.contains("no i2pd HTTP proxy at http://127.0.0.1:10/"), "{body}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

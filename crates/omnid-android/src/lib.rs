//! Runs the omnid daemon inside the Android app.
//!
//! The Kotlin side (`cc.wckd.omnid.Native`) starts it from a foreground
//! service, points a WebView at the port it returns, and pauses crawling
//! when the phone is off its charger or off an unmetered network. The daemon
//! is the same one `omnid serve` runs; only the defaults are phone-sized.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Mutex, Once};
use std::time::Duration;

use anyhow::{Context, Result};
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint};
use omnid::{Daemon, DaemonConfig};
use omnid_core::{Network, now_secs};
use omnid_crawl::{CrawlConfig, Politeness, Safety};
use omnid_net::GatewayConfig;

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static LOGGING: Once = Once::new();

struct Running {
    runtime: tokio::runtime::Runtime,
    daemon: Daemon,
}

/// Starts the daemon if it is not running and returns its port. Crawling
/// starts paused; the caller applies its battery and network policy with
/// [`set_paused`]. `hyphanet_node` is a Hyphanet node's FProxy and
/// `i2p_router` an i2pd HTTP proxy elsewhere (the owner's computer over
/// Tailscale, say), each empty for none. `speed` is how hard to crawl.
pub fn start(data_dir: &Path, token: &str, hyphanet_node: &str, i2p_router: &str, speed: CrawlSpeed) -> Result<u16> {
    start_with(data_dir, token, Routers { hyphanet_node, i2p_router }, speed, true)
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

/// What the app was told about routers elsewhere: addresses, or empty.
#[derive(Clone, Copy, Default)]
struct Routers<'a> {
    hyphanet_node: &'a str,
    i2p_router: &'a str,
}

/// `networks: false` leaves out Tor and the DHT, for tests.
fn start_with(data_dir: &Path, token: &str, routers: Routers, speed: CrawlSpeed, networks: bool) -> Result<u16> {
    let mut running = RUNNING.lock().expect("daemon lock poisoned");
    if let Some(running) = running.as_ref() {
        return Ok(running.daemon.addr().port());
    }
    LOGGING.call_once(init_logging);
    tracing::info!(?speed, "starting the daemon");

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
    let gateway = gateway(data_dir, routers, networks)?;

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
        socks: None,
        // The app's browser, behind the same secret as the API.
        browser_proxy: Some(token.to_owned()),
        // Magnets get their file lists; harvesting the whole DHT is a
        // server's job, not a phone's.
        dht: networks.then(|| omnid_dht::DhtConfig { max_peer_connections: 4, ..omnid_dht::DhtConfig::default() }),
        dht_harvest: false,
        abuse_blocklist: networks,
    }))?;
    let port = daemon.addr().port();
    *running = Some(Running { runtime, daemon });
    Ok(port)
}

/// The phone's gateway: Tor in the app's data directory, and the routers it
/// was given ahead of the phone's own: the i2pd app's proxy and a node's
/// FProxy on this phone, where they listen by default, stay as the fallback.
fn gateway(data_dir: &Path, routers: Routers, networks: bool) -> Result<GatewayConfig> {
    let mut gateway = GatewayConfig { tor_dir: networks.then(|| data_dir.join("tor")), ..GatewayConfig::default() };
    if let Some(node) = elsewhere(routers.hyphanet_node, "Hyphanet node", &["http", "https"])? {
        gateway.fproxies.retain(|n| *n != node);
        gateway.fproxies.insert(0, node);
    }
    // i2pd's proxy speaks plain HTTP only.
    if let Some(router) = elsewhere(routers.i2p_router, "I2P router", &["http"])? {
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

/// Stops crawling, commits the index and shuts the daemon down.
pub fn stop() {
    let Some(Running { runtime, daemon }) = RUNNING.lock().expect("daemon lock poisoned").take() else {
        return;
    };
    daemon.shutdown();
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
    hyphanet_node: JString<'local>,
    i2p_router: JString<'local>,
    crawl_speed: jint,
) -> jint {
    env.with_env(|_| -> Result<jint, NativeError> {
        let (node, router) = (hyphanet_node.to_string(), i2p_router.to_string());
        let speed = CrawlSpeed::from_ordinal(crawl_speed);
        let port = start(Path::new(&data_dir.to_string()), &token.to_string(), &node, &router, speed)
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

    fn urls(urls: &[url::Url]) -> Vec<&str> {
        urls.iter().map(url::Url::as_str).collect()
    }

    #[test]
    fn takes_routers_elsewhere_ahead_of_the_phones_own() {
        let dir = Path::new("unused");
        let local = gateway(dir, Routers { hyphanet_node: " ", i2p_router: "" }, false).unwrap();
        assert_eq!(urls(&local.fproxies), ["http://127.0.0.1:8888/"], "none given: a node on this phone");
        assert_eq!(urls(&local.i2p_proxies), ["http://127.0.0.1:4444/"], "and the i2pd app");

        let remote = gateway(
            dir,
            Routers { hyphanet_node: "http://100.101.102.103:8888/", i2p_router: "http://100.101.102.103:4444/" },
            false,
        )
        .unwrap();
        assert_eq!(urls(&remote.fproxies), ["http://100.101.102.103:8888/", "http://127.0.0.1:8888/"]);
        assert_eq!(urls(&remote.i2p_proxies), ["http://100.101.102.103:4444/", "http://127.0.0.1:4444/"]);

        let same = gateway(dir, Routers { i2p_router: "http://127.0.0.1:4444/", ..Routers::default() }, false).unwrap();
        assert_eq!(urls(&same.i2p_proxies), ["http://127.0.0.1:4444/"], "the phone's own, once");

        for node in ["ftp://100.101.102.103/", "100.101.102.103:8888", "not a url"] {
            assert!(gateway(dir, Routers { hyphanet_node: node, ..Routers::default() }, false).is_err(), "{node}");
        }
        for router in ["https://100.101.102.103:4444/", "100.101.102.103:4444"] {
            assert!(gateway(dir, Routers { i2p_router: router, ..Routers::default() }, false).is_err(), "{router}");
        }
    }

    /// A restart (a new Hyphanet node, Retry) stops a daemon whose Tor and
    /// DHT are running; on the phone a panic there kills the app.
    #[test]
    #[ignore = "live: reaches Tor and the DHT"]
    fn live_stops_with_tor_and_the_dht_running() {
        let dir = std::env::temp_dir().join(format!("omnid-android-live-{}", std::process::id()));
        start_with(&dir, "token", Routers::default(), CrawlSpeed::Normal, true).unwrap();
        std::thread::sleep(Duration::from_secs(20));
        stop();
        let again = start_with(&dir, "token", Routers::default(), CrawlSpeed::Normal, true).unwrap();
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
        let elsewhere = Routers { hyphanet_node: "http://127.0.0.1:9", i2p_router: "http://127.0.0.1:10" };
        let started = start_with(&dir, "token", elsewhere, CrawlSpeed::Fast, false).unwrap();
        assert_ne!(started, 0);
        let again = start_with(&dir, "token", Routers::default(), CrawlSpeed::Normal, false).unwrap();
        assert_eq!(again, started, "a second start reuses the daemon");
        assert_ne!(browser_proxy_port(), 0, "the browser's proxy is up");
        new_identity();
        set_paused(true, "waiting for a charger");
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", started)).unwrap();
        use std::io::{Read, Write};
        write!(stream, "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:{started}\r\nAuthorization: Bearer token\r\nConnection: close\r\n\r\n").unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        stop();
        assert_eq!(browser_proxy_port(), 0, "and down again");
        assert!(body.starts_with("HTTP/1.1 200"), "{body}");
        assert!(body.contains("waiting for a charger"), "{body}");
        assert!(body.contains("no FProxy at http://127.0.0.1:9/"), "{body}");
        assert!(body.contains("no i2pd HTTP proxy at http://127.0.0.1:10/"), "{body}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

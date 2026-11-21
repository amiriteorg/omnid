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
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlConfig, Job, Politeness, SEED_TRUST, Safety};
use omnid_net::GatewayConfig;

/// Crawled on first launch so a new install has something to search:
/// well-known services that publish onion mirrors of their own sites, and
/// I2P's own hubs, from which the crawl reaches the rest of I2P.
const FIRST_RUN_SEEDS: &[&str] = &[
    // The Tor Project
    "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/",
    // BBC News
    "https://www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion/",
    // I2P, crawled once the phone has a router (the i2pd app) and deferred
    // until then: the I2P Project, I2P Planet (news from eepsites across I2P)
    // and three name registries, which list the sites registered on it.
    "http://i2p-projekt.i2p/",
    "http://planet.i2p/",
    "http://reg.i2p/",
    "http://inr.i2p/",
    "http://stats.i2p/",
];

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static LOGGING: Once = Once::new();

struct Running {
    runtime: tokio::runtime::Runtime,
    daemon: Daemon,
}

/// Starts the daemon if it is not running and returns its port. Crawling
/// starts paused; the caller applies its battery and network policy with
/// [`set_paused`]. `hyphanet_node` is a Hyphanet node's FProxy elsewhere (the
/// owner's computer over Tailscale, say), or empty for one on this phone.
pub fn start(data_dir: &Path, token: &str, hyphanet_node: &str) -> Result<u16> {
    start_with(data_dir, token, hyphanet_node, true)
}

/// `networks: false` leaves out Tor and the DHT, for tests.
fn start_with(data_dir: &Path, token: &str, hyphanet_node: &str, networks: bool) -> Result<u16> {
    let mut running = RUNNING.lock().expect("daemon lock poisoned");
    if let Some(running) = running.as_ref() {
        return Ok(running.daemon.addr().port());
    }
    LOGGING.call_once(init_logging);

    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let first_run = omnid::open_store(data_dir)?.known()? == 0;
    let seeds = if first_run {
        FIRST_RUN_SEEDS
            .iter()
            .map(|s| Ok(Job { source: Source::parse(s)?, depth: 0, trust: SEED_TRUST }))
            .collect::<Result<_>>()?
    } else {
        vec![]
    };
    // Operators can drop lists into the app's data directory.
    let list = |name: &str| Some(data_dir.join(name)).filter(|p| p.exists());
    let safety = Safety::load(list("denylist.txt").as_deref(), list("blocked-terms.txt").as_deref())?;
    let gateway = gateway(data_dir, hyphanet_node, networks)?;

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
        crawl: phone_crawl(),
        safety,
        seeds,
        api_token: Some(token.to_owned()),
        start_paused: true,
        socks: None,
        // The app's browser, behind the same secret as the API.
        browser_proxy: Some(token.to_owned()),
        // Magnets get their file lists; harvesting the whole DHT is a
        // server's job, not a phone's.
        dht: networks.then(|| omnid_dht::DhtConfig { max_peer_connections: 4, ..omnid_dht::DhtConfig::default() }),
        dht_harvest: false,
    }))?;
    let port = daemon.addr().port();
    *running = Some(Running { runtime, daemon });
    Ok(port)
}

/// The phone's gateway: Tor in the app's data directory, and the Hyphanet
/// node it was given, if any.
fn gateway(data_dir: &Path, hyphanet_node: &str, networks: bool) -> Result<GatewayConfig> {
    let mut gateway = GatewayConfig { tor_dir: networks.then(|| data_dir.join("tor")), ..GatewayConfig::default() };
    let node = hyphanet_node.trim();
    if !node.is_empty() {
        let fproxy = gateway.fproxy.insert(node.parse().with_context(|| format!("Hyphanet node {node:?}"))?);
        anyhow::ensure!(
            matches!(fproxy.scheme(), "http" | "https") && fproxy.host().is_some(),
            "Hyphanet node {node:?} is not an http address"
        );
    }
    Ok(gateway)
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
/// a phone crawls in the background while it charges, not as a server.
fn phone_crawl() -> CrawlConfig {
    let politeness = |concurrency, delay_secs| Politeness { concurrency, site_delay: Duration::from_secs(delay_secs) };
    CrawlConfig {
        max_depth: 2,
        max_page_bytes: 1 << 20,
        frontier_capacity: 5_000,
        commit_interval: Duration::from_secs(30),
        politeness: HashMap::from([
            (Network::Clearnet, politeness(6, 1)),
            (Network::Onion, politeness(6, 5)),
            (Network::I2p, politeness(2, 5)),
            (Network::Hyphanet, politeness(1, 2)),
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
) -> jint {
    env.with_env(|_| -> Result<jint, NativeError> {
        let port = start(Path::new(&data_dir.to_string()), &token.to_string(), &hyphanet_node.to_string())
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
    fn first_run_seeds_parse() {
        for seed in FIRST_RUN_SEEDS {
            let network = Source::parse(seed).unwrap().network();
            assert!(matches!(network, Network::Onion | Network::I2p), "{seed}: overlay sites only");
        }
        assert!(FIRST_RUN_SEEDS.iter().any(|s| s.contains(".i2p")), "I2P has seeds of its own");
    }

    #[test]
    fn takes_a_hyphanet_node_elsewhere() {
        let dir = Path::new("unused");
        let local = gateway(dir, " ", false).unwrap().fproxy.unwrap();
        assert_eq!(local.as_str(), "http://127.0.0.1:8888/", "none given: a node on this phone");
        let remote = gateway(dir, "http://100.101.102.103:8888/", false).unwrap().fproxy.unwrap();
        assert_eq!(remote.as_str(), "http://100.101.102.103:8888/");
        for node in ["ftp://100.101.102.103/", "100.101.102.103:8888", "not a url"] {
            assert!(gateway(dir, node, false).is_err(), "{node}");
        }
    }

    /// A restart (a new Hyphanet node, Retry) stops a daemon whose Tor and
    /// DHT are running; on the phone a panic there kills the app.
    #[test]
    #[ignore = "live: reaches Tor and the DHT"]
    fn live_stops_with_tor_and_the_dht_running() {
        let dir = std::env::temp_dir().join(format!("omnid-android-live-{}", std::process::id()));
        start_with(&dir, "token", "", true).unwrap();
        std::thread::sleep(Duration::from_secs(20));
        stop();
        let again = start_with(&dir, "token", "", true).unwrap();
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
        let started = start_with(&dir, "token", "http://127.0.0.1:9", false).unwrap();
        assert_ne!(started, 0);
        assert_eq!(start_with(&dir, "token", "", false).unwrap(), started, "a second start reuses the daemon");
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
        let _ = std::fs::remove_dir_all(dir);
    }
}

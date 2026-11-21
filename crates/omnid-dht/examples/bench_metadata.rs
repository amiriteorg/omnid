//! Times `Dht::metadata` on openly licensed torrents against the real DHT,
//! to compare changes to the lookup and the peer exchange.
//!
//!     cargo run --release -p omnid-dht --example bench_metadata -- 5           # rounds
//!     cargo run --release -p omnid-dht --example bench_metadata -- 5 --fresh
//!
//! Shared, the default: one `Dht` answers every call, as in the daemon. It
//! bootstraps for 30 s before the clock starts, so every call sees a settled
//! routing table. `--fresh`: a new `Dht` per call, timed from `Dht::start`,
//! bootstrap included, as `omnid fetch` runs. Each round asks for every
//! torrent once, in a rotating order, one call at a time. Prints every call,
//! then per torrent the min, median and max seconds of the calls that
//! succeeded, and the median peer count. `RUST_LOG=omnid_dht=debug` adds
//! the crate's own timeline: lookup done, metadata received.

use std::time::{Duration, Instant};

use omnid_dht::{Dht, DhtConfig};

/// Blender Foundation films (CC-BY), seeded for WebTorrent's demos.
const TORRENTS: [(&str, &str); 3] = [
    ("Sintel", "08ada5a7a6183aae1e09d831df6748d566095a10"),
    ("Big Buck Bunny", "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"),
    ("Tears of Steel", "209c8226b299b308beaf2b9cd3fb49212dbd13ec"),
];
const WARM_UP: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Samples {
    seconds: Vec<f64>,
    peers: Vec<usize>,
    failures: Vec<String>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fresh = args.iter().any(|a| a == "--fresh");
    let rounds: usize = args.iter().find_map(|a| a.parse().ok()).unwrap_or(5);
    let cfg = DhtConfig::default();
    println!(
        "mode={} rounds={rounds} max_queries_per_sec={} max_peer_connections={} metadata_timeout={:?}",
        if fresh { "fresh" } else { "shared" },
        cfg.max_queries_per_sec,
        cfg.max_peer_connections,
        cfg.metadata_timeout,
    );
    let shared = if fresh {
        None
    } else {
        let dht = Dht::start(cfg.clone()).await?;
        tokio::time::sleep(WARM_UP).await;
        println!("warmed up for {WARM_UP:?}: {:?}", dht.status());
        Some(dht)
    };

    let mut samples: Vec<Samples> = TORRENTS.iter().map(|_| Samples::default()).collect();
    for round in 1..=rounds {
        for i in 0..TORRENTS.len() {
            let t = (round + i) % TORRENTS.len();
            let (name, hex) = TORRENTS[t];
            let start = Instant::now();
            let dht = match &shared {
                Some(dht) => dht.clone(),
                None => Dht::start(cfg.clone()).await?,
            };
            let result = dht.metadata(info_hash(hex)).await;
            let seconds = start.elapsed().as_secs_f64();
            let s = &mut samples[t];
            match result {
                Ok(meta) => {
                    println!("round {round} {name:<14} {seconds:6.2}s  {:>3} peers  {:?}", meta.peers, dht.status());
                    s.seconds.push(seconds);
                    s.peers.push(meta.peers);
                }
                Err(e) => {
                    println!("round {round} {name:<14} {seconds:6.2}s  failed: {e}  {:?}", dht.status());
                    s.failures.push(format!("{seconds:.2}s: {e}"));
                }
            }
        }
    }

    println!();
    println!("{:<14} {:>5} {:>7} {:>7} {:>7} {:>6}  raw seconds", "torrent", "ok", "min", "median", "max", "peers");
    for ((name, _), s) in TORRENTS.iter().zip(&samples) {
        let mut sorted = s.seconds.clone();
        sorted.sort_by(f64::total_cmp);
        let mut peers = s.peers.clone();
        peers.sort_unstable();
        let fmt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.2}"));
        let raw: Vec<String> = s.seconds.iter().map(|v| format!("{v:.2}")).collect();
        println!(
            "{name:<14} {:>5} {:>7} {:>7} {:>7} {:>6}  [{}]",
            format!("{}/{}", s.seconds.len(), s.seconds.len() + s.failures.len()),
            fmt(sorted.first().copied()),
            fmt(median(&sorted)),
            fmt(sorted.last().copied()),
            median(&peers.iter().map(|&p| p as f64).collect::<Vec<_>>()).map_or("-".into(), |p| format!("{p:.0}")),
            raw.join(", "),
        );
        for failure in &s.failures {
            println!("{:<14} failed after {failure}", "");
        }
    }
    Ok(())
}

/// Of sorted values.
fn median(sorted: &[f64]) -> Option<f64> {
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

fn info_hash(hex: &str) -> [u8; 20] {
    std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
}

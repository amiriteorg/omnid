//! Download time for one file over Tor with several connection counts. The
//! counts take turns, round after round, so the network's drift falls on all
//! of them alike; every download must be byte-identical to the first. Needs
//! the network.
//!
//!     cargo run --release -p omnid-net --example bench_download -- <url> [samples] [connections,...]
//!
//! An onion stays inside Tor; a clearnet URL goes through exits
//! (`clearnet_via_tor`).

use std::collections::BTreeMap;
use std::fs::File;
use std::time::{Duration, Instant};

use omnid_core::Source;
use omnid_net::{DownloadOptions, FetchOptions, Gateway, GatewayConfig, Progress};
use tokio_util::sync::CancellationToken;
use url::Url;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let url: Url = args.next().expect("usage: bench_download <url> [samples] [connections,...]").parse().unwrap();
    let samples: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let counts: Vec<usize> =
        args.next().map(|a| a.split(',').map(|n| n.parse().unwrap()).collect()).unwrap_or_else(|| vec![1, 4, 8]);
    let dir =
        std::env::var("OMNID_TOR_DIR").map(Into::into).unwrap_or_else(|_| std::env::temp_dir().join("omnid-bench-tor"));
    let gateway =
        Gateway::new(GatewayConfig { tor_dir: Some(dir), clearnet_via_tor: true, ..GatewayConfig::default() }).unwrap();

    // The bootstrap is not part of any download: a small page first waits
    // for it.
    let started = Instant::now();
    let root = Source::parse(url.join("/").unwrap().as_str()).unwrap();
    let opts = FetchOptions { max_bytes: 4 << 10, ..FetchOptions::default() };
    if let Err(e) = gateway.fetch(&root, &opts).await {
        eprintln!("{root}: {e}");
    }
    println!("bootstrap + first fetch: {:.1} s", started.elapsed().as_secs_f64());

    let out = std::env::temp_dir().join(format!("omnid-bench-download-{}", std::process::id()));
    let mut reference: Option<Vec<u8>> = None;
    let mut times: BTreeMap<usize, Vec<Duration>> = BTreeMap::new();
    let mut failures: BTreeMap<usize, usize> = BTreeMap::new();
    for round in 1..=samples {
        for &connections in &counts {
            let file = File::create(&out).unwrap();
            let opts = DownloadOptions { connections, ..DownloadOptions::default() };
            let progress = Progress::default();
            let t = Instant::now();
            match gateway.download(&url, &file, &opts, &progress, &CancellationToken::new()).await {
                Ok(download) => {
                    let elapsed = t.elapsed();
                    let bytes = std::fs::read(&out).unwrap();
                    match &reference {
                        Some(reference) => {
                            assert!(*reference == bytes, "round {round}, {connections}: not the same bytes")
                        }
                        None => reference = Some(bytes),
                    }
                    println!(
                        "round {round} connections {connections:>2}: {:6.1} s {:6.2} MB/s, {} open at most, {} retries{}",
                        elapsed.as_secs_f64(),
                        download.bytes as f64 / 1e6 / elapsed.as_secs_f64(),
                        download.connections,
                        download.retries,
                        if download.ranged { "" } else { ", no ranges" },
                    );
                    times.entry(connections).or_default().push(elapsed);
                }
                Err(e) => {
                    eprintln!("round {round} connections {connections}: {e}");
                    *failures.entry(connections).or_default() += 1;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&out);

    let size = reference.map_or(0, |r| r.len()) as f64 / 1e6;
    println!("\n{size:.1} MB, {url}");
    for (connections, mut times) in times {
        times.sort();
        let s = |d: Duration| d.as_secs_f64();
        let n = times.len();
        let median = if n % 2 == 1 { s(times[n / 2]) } else { (s(times[n / 2 - 1]) + s(times[n / 2])) / 2.0 };
        // Nearest rank: the maximum below 20 samples.
        let p95 = s(times[(n * 95).div_ceil(100).max(1) - 1]);
        println!(
            "connections {connections:>2}: n {n}, {} failed, median {median:.1} s ({:.2} MB/s), p95 {p95:.1} s, min {:.1}, max {:.1}",
            failures.get(&connections).copied().unwrap_or(0),
            size / median,
            s(times[0]),
            s(times[n - 1]),
        );
    }
}

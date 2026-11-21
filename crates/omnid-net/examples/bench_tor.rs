//! Latency of sequential page fetches from one onion service over Tor, the
//! pattern a crawl produces (politeness delay left out). Needs the network.
//!
//!     cargo run --release -p omnid-net --example bench_tor -- [rounds]

use std::time::{Duration, Instant};

use omnid_core::Source;
use omnid_net::{FetchOptions, Gateway, GatewayConfig};

/// Small, stable pages on two sites that publish onion services: one plain
/// HTTP, one HTTPS.
const SITES: [(&str, &[&str]); 2] = [
    (
        "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion",
        &["/", "/download/", "/about/history/", "/contact/", "/donate/", "/community/"],
    ),
    (
        "https://www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion",
        &["/news", "/sport", "/weather", "/news/world", "/news/technology", "/news/science_and_environment"],
    ),
];

#[tokio::main]
async fn main() {
    let rounds: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(3);
    let dir =
        std::env::var("OMNID_TOR_DIR").map(Into::into).unwrap_or_else(|_| std::env::temp_dir().join("omnid-bench-tor"));
    let started = Instant::now();
    let gateway = Gateway::new(GatewayConfig { tor_dir: Some(dir), ..GatewayConfig::default() }).unwrap();
    // The first fetch waits for the bootstrap; time it separately.
    gateway.fetch(&Source::parse(SITES[0].0).unwrap(), &FetchOptions::default()).await.unwrap();
    println!("bootstrap + first fetch: {:.1} s", started.elapsed().as_secs_f64());

    for (site, paths) in SITES {
        let mut times: Vec<Duration> = Vec::new();
        let mut failures = 0;
        for _ in 0..rounds {
            for path in paths {
                let source = Source::parse(&format!("{site}{path}")).unwrap();
                let t = Instant::now();
                match gateway.fetch(&source, &FetchOptions::default()).await {
                    Ok(_) => times.push(t.elapsed()),
                    Err(e) => {
                        failures += 1;
                        eprintln!("{source}: {e}");
                    }
                }
            }
        }
        times.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let n = times.len();
        println!(
            "{site}: {n} fetches, {failures} failed, p50 {:.0} ms, p95 {:.0} ms, min {:.0}, max {:.0}",
            ms(times[n / 2]),
            ms(times[(n * 95 / 100).min(n - 1)]),
            ms(times[0]),
            ms(times[n - 1])
        );
    }
}

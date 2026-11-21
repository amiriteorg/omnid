//! Runs the DHT for a while, resolving openly licensed torrents once a
//! minute and printing the routing table's state, to exercise its upkeep
//! (pings, 15-minute refreshes) over a realistic stretch.
//!
//!     cargo run --release -p omnid-dht --example soak -- 30   # minutes

use std::time::{Duration, Instant};

use omnid_dht::{Dht, DhtConfig};

/// Blender Foundation films (CC-BY), seeded for WebTorrent's demos.
const TORRENTS: [(&str, &str); 3] = [
    ("Sintel", "08ada5a7a6183aae1e09d831df6748d566095a10"),
    ("Big Buck Bunny", "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"),
    ("Tears of Steel", "209c8226b299b308beaf2b9cd3fb49212dbd13ec"),
];

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let minutes: u64 = std::env::args().nth(1).and_then(|m| m.parse().ok()).unwrap_or(30);
    let dht = Dht::start(DhtConfig::default()).await?;
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    for round in 0.. {
        tick.tick().await;
        if started.elapsed() >= Duration::from_secs(minutes * 60) {
            break;
        }
        let (name, hex) = TORRENTS[round % TORRENTS.len()];
        let info_hash: [u8; 20] = std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap());
        let lookup = Instant::now();
        let result = match tokio::time::timeout(Duration::from_secs(55), dht.metadata(info_hash)).await {
            Ok(Ok(meta)) => format!("{} files, {} peers", meta.files.len(), meta.peers),
            Ok(Err(e)) => format!("failed: {e}"),
            Err(_) => "timed out".into(),
        };
        let status = dht.status();
        println!(
            "{:>5}s nodes={:<4} ready={} sent={:<6} answered={:<6} {name}: {result} in {:.1}s",
            started.elapsed().as_secs(),
            status.nodes,
            status.ready,
            status.queries_sent,
            status.responses,
            lookup.elapsed().as_secs_f32(),
        );
    }
    Ok(())
}

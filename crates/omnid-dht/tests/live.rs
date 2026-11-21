//! Against the real DHT, so ignored by default. Run with
//! `cargo test -p omnid-dht -- --ignored --nocapture`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use omnid_dht::{Dht, DhtConfig};

/// Sintel, the Blender Foundation's CC-BY film that WebTorrent uses in its
/// examples.
const SINTEL: &str = "08ada5a7a6183aae1e09d831df6748d566095a10";

fn info_hash(hex: &str) -> [u8; 20] {
    let mut hash = [0; 20];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    hash
}

async fn wait_ready(dht: &Dht, limit: Duration) {
    let start = Instant::now();
    while !dht.status().ready && start.elapsed() < limit {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
#[ignore = "needs the internet and UDP"]
async fn fetches_sintel_metadata() {
    let start = Instant::now();
    let dht = Dht::start(DhtConfig::default()).await.unwrap();
    let meta = dht.metadata(info_hash(SINTEL)).await;
    println!("metadata after {:.1?}: {:?}", start.elapsed(), dht.status());
    let meta = meta.unwrap();
    println!("name {:?}, {} peers, {} files, {} bytes", meta.name, meta.peers, meta.files.len(), meta.total_size);
    for file in &meta.files {
        println!("  {} ({} bytes)", file.path, file.size);
    }
    assert_eq!(meta.name, "Sintel");
    assert!(meta.files.len() > 1);
    assert!(meta.peers > 0);
}

#[tokio::test]
#[ignore = "needs the internet and UDP"]
async fn harvests_for_thirty_seconds() {
    let start = Instant::now();
    let dht = Dht::start(DhtConfig::default()).await.unwrap();
    wait_ready(&dht, Duration::from_secs(30)).await;
    println!("bootstrapped after {:.1?}: {:?}", start.elapsed(), dht.status());

    let harvest_start = Instant::now();
    let mut harvest = dht.harvest();
    let mut hashes = HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            hash = harvest.recv() => match hash {
                Some(hash) => { hashes.insert(hash); }
                None => break,
            },
        }
    }
    println!("harvest: {} distinct info hashes in {:.1?}; {:?}", hashes.len(), harvest_start.elapsed(), dht.status());
    assert!(!hashes.is_empty());
}

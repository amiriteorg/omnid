//! Search latency on a synthetic index: per query shape, p50 and p95 over
//! many runs. The index is built once in a temporary directory.
//!
//!     cargo run --release -p omnid-index --example bench_search -- [docs] [runs]
//!
//! Pages declare their language, as most crawled pages do: one in four German,
//! the rest English. `BENCH_LANGUAGES=en` sends a reader's language with every
//! query.

use std::time::{Duration, Instant};

use omnid_core::{IndexedDoc, Network};
use omnid_index::{SearchIndex, SearchQuery};

const WORDS: [&str; 64] = [
    "onion",
    "service",
    "mirror",
    "index",
    "archive",
    "library",
    "forum",
    "market",
    "privacy",
    "relay",
    "circuit",
    "network",
    "search",
    "crawler",
    "garlic",
    "tunnel",
    "freesite",
    "torrent",
    "magnet",
    "peer",
    "swarm",
    "hash",
    "router",
    "gateway",
    "directory",
    "wiki",
    "blog",
    "news",
    "guide",
    "manual",
    "release",
    "download",
    "debian",
    "linux",
    "kernel",
    "package",
    "mail",
    "chat",
    "secure",
    "drop",
    "journalism",
    "leak",
    "document",
    "report",
    "science",
    "paper",
    "book",
    "music",
    "video",
    "film",
    "open",
    "source",
    "code",
    "project",
    "community",
    "help",
    "support",
    "contact",
    "about",
    "history",
    "update",
    "version",
    "stable",
    "rare",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// Skewed towards the start of WORDS, like real text; "rare" is word 63.
    fn word(&mut self) -> &'static str {
        let r = self.next() % 1000;
        WORDS[((r * r) / 15_700) as usize % WORDS.len()]
    }

    fn text(&mut self, words: usize) -> String {
        (0..words).map(|_| self.word()).collect::<Vec<_>>().join(" ")
    }
}

fn build(docs: usize) -> (tempfile::TempDir, SearchIndex) {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path()).unwrap();
    let mut rng = Rng(42);
    for i in 0..docs {
        let network = Network::ALL[i % 4];
        // One document in ten mirrors the previous one on another network.
        let hash = if i % 10 == 9 { format!("h{}", i - 1) } else { format!("h{i}") };
        index
            .upsert(&IndexedDoc {
                url: format!("http://site{}.example/{i}", i % 500),
                network,
                site: format!("site{}", i % 500),
                title: rng.text(6),
                description: rng.text(25),
                keywords: vec![],
                body: rng.text(300),
                content_hash: hash,
                info_hash: None,
                files: vec![],
                first_seen: 1_000,
                last_seen: 1_000,
                liveness: (rng.next() % 100) as f32 / 100.0,
                trust: 0.5,
                lang: Some(if i % 4 == 3 { "de" } else { "en" }.into()),
            })
            .unwrap();
    }
    index.commit().unwrap();
    (dir, index)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let docs: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(50_000);
    let runs: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(200);

    let started = Instant::now();
    let (_dir, index) = build(docs);
    println!("indexed {docs} docs in {:.1} s", started.elapsed().as_secs_f64());

    let shapes: [(&str, SearchQuery); 7] = [
        ("common word", SearchQuery { text: "onion".into(), limit: 20, ..Default::default() }),
        ("rare word", SearchQuery { text: "rare".into(), limit: 20, ..Default::default() }),
        ("two words", SearchQuery { text: "privacy network".into(), limit: 20, ..Default::default() }),
        ("phrase", SearchQuery { text: "\"onion service\"".into(), limit: 20, ..Default::default() }),
        (
            "network filter",
            SearchQuery { text: "".into(), networks: vec![Network::I2p], limit: 20, ..Default::default() },
        ),
        ("any-word fallback", SearchQuery { text: "nonexistent rare".into(), limit: 20, ..Default::default() }),
        ("page 5", SearchQuery { text: "mirror".into(), offset: 80, limit: 20, ..Default::default() }),
    ];
    let languages: Vec<String> =
        std::env::var("BENCH_LANGUAGES").map(|l| l.split(',').map(str::to_owned).collect()).unwrap_or_default();
    println!("{:<18} {:>9} {:>9} {:>9}", "query", "p50 ms", "p95 ms", "max ms");
    for (name, query) in shapes {
        let query = &SearchQuery { languages: languages.clone(), ..query };
        // Warm-up, then timed runs.
        for _ in 0..5 {
            index.search(query).unwrap();
        }
        let mut times: Vec<Duration> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(index.search(query).unwrap());
                t.elapsed()
            })
            .collect();
        times.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{name:<18} {:>9.2} {:>9.2} {:>9.2}",
            ms(times[runs / 2]),
            ms(times[runs * 95 / 100]),
            ms(times[runs - 1])
        );
    }
}

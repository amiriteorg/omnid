# Benchmarks

Every number here came from the command next to it, on the machine named,
release profile. Re-run the same command before and after a change; a single
run is not a result.

Machine: Apple M3 Pro, 12 cores, macOS (Darwin 27.2), Rust 1.99.1.

## Crawl throughput (local, no network latency)

    cargo run --release -p omnid-crawl --example bench_crawl -- 2000 5

A synthetic site of 2,000 deterministic ~8 KB pages with ten internal links
each, served over loopback; on-disk store and index; politeness delay off.
One site, so pages go one at a time. Measures the crawler's own cost per page.

| Date | Change | pages/s min / median / max (5 rounds) |
|---|---|---|
| 2026-11-21 | baseline | 348 / 362 / 364 |
| 2026-11-21 | no 5 s nap when the working set runs dry | 3,408 / 4,105 / 4,403 |

The baseline's 5.5 s per round was ~0.24 ms per page plus a fixed ~5 s nap at
the end of every crawl (6,000 pages took 6.4 s), confirmed by the regression
test `drains_a_store_bigger_than_the_working_set_without_stalling` (30 s
timeout without the fix, under a second with it).

## Search latency

    cargo run --release -p omnid-index --example bench_search -- 50000 200

50,000 synthetic documents (6-word titles, 25-word descriptions, 300-word
bodies from a skewed 64-word vocabulary, one in ten a mirror, four networks),
built once; each query shape warmed up 5 times, then timed 200 times.

| Query | p50 / p95 ms, baseline | p50 / p95 ms, one pass (2 runs) |
|---|---|---|
| common word | 1.94 / 2.04 | 1.84–1.94 / 1.91–2.22 |
| rare word | 2.28 / 2.38 | 1.58–1.60 / 1.65–1.66 |
| two words | 4.87 / 5.09 | 2.28–2.39 / 2.31–2.66 |
| "phrase" | 60.22 / 62.89 | 15.69–15.72 / 16.56–16.69 |
| network filter (empty text) | 1.22 / 1.37 | 1.18–1.21 / 1.20–1.34 |
| any-word fallback | 2.46 / 4.51 | 1.67–1.74 / 1.72–2.09 |
| page 5 | 2.02 / 2.68 | 1.96–1.97 / 2.03–2.05 |

The baseline evaluated the text query seven times per search: once per
network for the facet counts, once for the hits. Removing the counts alone
took phrases to 32 ms. Now the text query runs once, collecting the top
documents and a bitset of all matches together, and each network's count
comes from walking its postings against that bitset.

## Sequential onion fetches over Tor

    cargo run --release -p omnid-net --example bench_tor -- 3

Six pages each from the Tor Project's onion (HTTP) and the BBC's (HTTPS),
fetched one after another three times (18 fetches per site per run), after
the bootstrap. Live network: Tor latency varies minute to minute, so the
old and new binaries were run alternately (base, pooled, base, pooled) on
the same warm Tor state.

| Site | p50 ms, no reuse | p50 ms, keep-alive | min ms, no reuse | min ms, keep-alive |
|---|---|---|---|---|
| Tor Project, HTTP | 617, 525 | 315, 154 | 348, 367 | 163, 76 |
| BBC, HTTPS | 1,128, 873 | 444, 521 | 581, 526 | 144, 284 |

p95 (1.3–4.2 s) is network jitter in both and does not separate them.

## DHT metadata

    cargo run --release -p omnid-dht --example bench_metadata -- 5           # one shared Dht, warmed 30 s
    cargo run --release -p omnid-dht --example bench_metadata -- 5 --fresh   # a new Dht per call, bootstrap included

Sintel, Big Buck Bunny and Tears of Steel (CC-BY Blender films), one call at
a time, against the live DHT. Builds alternated (A, B, A, B) because swarm
and routing conditions drift; N = 10 calls per torrent per side.

Pipelining (ask peers while the get_peers lookup still runs), median of all
30 calls: shared 4.08 -> 2.97 s, fresh 8.11 -> 6.42 s. Per-torrent tables
and raw times are in the commit message of "Ask peers for metadata while the
get_peers lookup is still running".

Ending the post-metadata peer count after 1 s without a new peer (cap still
2 s), shared:

| Torrent | median s, 2 s count | median s, quiet 1 s | raw, quiet 1 s |
|---|---|---|---|
| Sintel | 3.23 | 2.74 | 2.91 2.66 2.87 10.72 2.81 2.54 2.40 3.78 1.97 2.13 |
| Big Buck Bunny | 6.44 | 3.14 | 7.30 2.41 2.82 2.47 2.40 7.59 11.43 16.15 3.46 1.51 |
| Tears of Steel | 2.43 | 2.08 | 2.05 1.61 2.22 1.69 1.54 2.37 2.10 1.71 2.44 3.00 |
| all 30 | 3.03 (mean 5.30) | 2.46 (mean 3.77) | |

Big Buck Bunny's tail is dead peers (5 s connect, 10 s read timeouts), as
in the baseline, so its median moves with whichever peers the DHT returns.
Median peer counts did not drop (Sintel 274/344 -> 269/347).

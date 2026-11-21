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

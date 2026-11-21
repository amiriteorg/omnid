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

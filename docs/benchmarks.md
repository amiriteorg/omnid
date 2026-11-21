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
| 2026-11-21 | page languages (detected here: the pages declare none) | medians 3,171–4,411 vs 3,994–4,433 before, 5 alternating runs each |

Detecting a page's language costs about 44 µs (50,000 undeclared pages took
5.6 s to index instead of 3.4 s), which the crawl does not show: no
measurable change. The one slow median (3,171) ran at a load average of 12.

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

Ranking by language and depth (2026-11-21). The pages now declare a language,
one in four German; `BENCH_LANGUAGES=en` sends a reader's language with every
query. Two alternating runs each, p50 / p95 ms:

| Query | before | four columns | one prior column | four columns, `en` | one prior column, `en` |
|---|---|---|---|---|---|
| common word | 1.86 / 1.95–1.96 | 2.05–2.13 / 2.17–2.26 | 1.69–1.73 / 1.85–1.95 | 2.11–2.14 / 2.22–2.31 | 1.84–1.91 / 2.77–3.34 |
| rare word | 1.58 / 1.69–1.71 | 1.79 / 1.86–1.92 | 1.46–1.47 / 1.51–1.62 | 1.89–1.91 / 2.01–2.08 | 1.65–1.67 / 1.80–1.88 |
| two words | 2.29–2.32 / 2.43–2.54 | 2.46–2.54 / 2.61–2.86 | 2.19–2.24 / 2.46–3.19 | 2.72–2.84 / 3.02–3.65 | 2.38–2.40 / 2.52–2.57 |
| "phrase" | 16.02–16.32 / 16.89–17.87 | 15.88–16.31 / 16.76–17.43 | 15.81–16.08 / 16.81–16.83 | 16.22–16.66 / 17.23–18.31 | 16.07–16.16 / 16.86–16.91 |
| any-word fallback | 1.67–1.73 / 1.75–1.84 | 1.82–1.85 / 1.93–2.00 | 1.57–1.66 / 1.62–1.95 | 1.99–2.00 / 2.12–2.16 | 1.72–1.74 / 1.84–1.85 |
| page 5 | 1.94–1.97 / 2.04–2.13 | 2.11–2.15 / 2.26–2.27 | 1.78–1.82 / 1.94–2.41 | 2.19–2.24 / 2.31–2.43 | 1.88–1.91 / 2.01–2.04 |

Reading trust, liveness, depth and language per matching document cost
0.2–0.5 ms. Trust, liveness and depth change only when a document is
written, so they are multiplied into one `prior` column then; ranking reads
that and, when the reader names languages, the language. The machine was
under other load during these runs (load average 10–12).

The word being typed (2026-11-21, Apple M5 Pro, the Mac mini). The last
plain word of three letters or more also matches the words it starts, as a
prefix query over the five text fields alongside the query as typed, so
results appear before a word is finished. 100,000 documents, 300 timed runs
per shape, three runs each, p50 / p95 ms:

    cargo run --release -p omnid-index --example bench_search -- 100000 300

| Query | before | prefix on the last word |
|---|---|---|
| common word | 1.77–1.79 / 1.79–1.80 | 3.16–3.22 / 3.25 |
| rare word | 1.38–1.40 / 1.41–1.43 | 2.17–2.18 / 2.21–2.22 |
| two words | 2.46–2.51 / 2.52–2.57 | 4.36–4.38 / 4.40–4.44 |
| "phrase" | 24.18–25.64 / 25.06–25.99 | 23.93–24.32 / 24.76–25.21 |
| any-word fallback | 1.51–1.57 / 1.54–1.62 | 2.41–2.46 / 2.47–2.49 |

About 0.8–1.9 ms more per query, from walking the prefix's terms; phrases
are unchanged (a phrase's last word is not a plain word). Accepted: a search
still answers in under 5 ms at this size, and before it a half-typed word
found nothing. The close-spelling pass runs only when nothing matches, so it
adds nothing to a search that finds something.

## Ranking quality

    cargo run --release -p omnid --example eval_ranking -- fetch target/eval/torproject 400
    EVAL_LANGUAGES=en cargo run --release -p omnid --example eval_ranking -- run target/eval/torproject

The Tor Project's onion fetched once over Tor on 2026-11-21 (193 pages,
robots.txt honoured, two links deep): 18 English pages and about 6 pages in
each of 29 translations, every one declaring its `<html lang>`. `run` serves
the cache over loopback and indexes it with the real crawler, so every
version ranks identical input. 19 English queries, judged from the English
pages' titles and headings before any run (`JUDGMENTS` in the example).

| Ranking | success@1 | MRR@10 | translations in the top 3 |
|---|---|---|---|
| before (3 runs; ties fall in crawl order) | 11–13 / 19 | 0.702–0.756 | 12–13 / 57 |
| depth, no language preference | 17 / 19 | 0.904 | 11–13 / 57 |
| depth and the reader's language (`en`) | 18 / 19 | 0.953 | 8 / 57 |

Before, "tor browser" put `/press/page/5/` first and the home page sixth, and
"history" put the Czech history page first. The remaining miss is "tor
project": every page's title starts "Tor Project |", and the home page comes
tenth behind pages that repeat the name in their text. Translations still
fill places 2 and 3 when a query has one English answer; a language the
reader did not ask for is a 0.7 factor, not a filter.

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

## Downloads over Tor

    OMNID_TOR_DIR=target/bench/tor cargo run --release -p omnid-net --example bench_download -- \
      http://2s4yqjx5ul6okpp3f2gaunr2syex5jgbfpfvhxxbbjwnrsvbk5v3qbid.onion/debian/pool/main/l/llvm-toolchain-14/libllvm14_14.0.6-12_amd64.deb 5 1,4,8
    OMNID_TOR_DIR=target/bench/tor cargo run --release -p omnid-net --example bench_download -- \
      https://www.python.org/ftp/python/3.12.0/Python-3.12.0.tar.xz 5 1,4,8

2026-11-21, the Mac mini (Apple M5 Pro), Rust 1.99.1. Two files that do not
change: Debian 12's `libllvm14` package from Debian's onion mirror (21.8 MB,
inside Tor), and Python 3.12.0's source from python.org through Tor exits
(20.6 MB, `clearnet_via_tor`). One gateway, warm after the bootstrap; the
counts take turns (1, 4, 8, 1, 4, 8, ...) so the network's drift falls on all
of them; every connection is on circuits of its own, so each download builds
new ones. Defaults otherwise: parts of 1 MiB at least, 30 s stall timeout.
Every download was byte-identical to the first, and the onion's copy to
deb.debian.org's (sha256 `cd986403…cf64a8`). Other builds ran on the machine
(load average 5–12); the downloads used 4–11 % of a core. About 640 MB in all.

| File | connections | n | median s (MB/s) | p95 s | min–max s | retries |
|---|---|---|---|---|---|---|
| onion, 21.8 MB | 1 | 5 | 158.6 (0.14) | 317.6 | 127.3–317.6 | 1 |
| | 4 | 5 | 63.5 (0.34) | 97.6 | 49.6–97.6 | 1 |
| | 8 | 5 | 52.5 (0.42) | 60.4 | 42.9–60.4 | 14 |
| exits, 20.6 MB | 1 | 5 | 18.5 (1.11) | 33.9 | 6.8–33.9 | 0 |
| | 4 | 5 | 8.2 (2.51) | 9.1 | 5.3–9.1 | 0 |
| | 8 | 5 | 9.6 (2.15) | 10.9 | 5.6–10.9 | 0 |

p95 is nearest-rank, so with 5 samples it is the slowest. Seconds by round:

    onion  1: 147.7 127.3 317.6 158.6 199.7   4: 49.6 54.6 85.6 97.6 63.5   8: 52.5 55.8 49.3 60.4 42.9
    exits  1:   6.8  33.9  28.9   9.3  18.5   4:  8.2  9.1  5.9  8.4  5.3   8:  5.6  9.6 10.1 10.9  9.0

To the onion, 4 connections against 1 is outside the noise: every
4-connection download (97.6 s at worst) beat every 1-connection one (127.3 s
at best), median 2.5x faster. 8 against 4 is within it: medians 52.5 and
63.5 s, ranges overlapping (42.9–60.4 and 49.6–97.6), though 8's spread was
the narrowest. Through exits one connection's time is whichever exit it got
(6.8 to 33.9 s); with 4 the slowest was 9.1 s. The median gain (2.3x) is not
separated from the noise at this N: the best single-connection run beat
three of the 4-connection ones. The tail is: 33.9 s at worst against 9.1. 8
connections were no faster than 4 (medians 9.6 and 8.2 s, within noise):
each gets 2.6 MB of a 20.6 MB file, and opening eight connections costs as
much as it saves. The default stays at 4.

All retries recovered (14 of them in two 8-connection onion runs). Their
cause was not captured: the example has no log subscriber, and a rerun of
`omnid download --connections 8` on the onion with `RUST_LOG=omnid_net=debug`
(48.4 s) had none.

## DHT metadata

    cargo run --release -p omnid-dht --example bench_metadata -- 5           # one shared Dht, warmed 30 s
    cargo run --release -p omnid-dht --example bench_metadata -- 5 --fresh   # a new Dht per call, bootstrap included

Sintel, Big Buck Bunny and Tears of Steel (CC-BY Blender films), one call at
a time, against the live DHT. Builds alternated (A, B, A, B) because swarm
and routing conditions drift; N = 10 calls per torrent per side.

Pipelining (ask peers while the get_peers lookup still runs), median of all
30 calls: shared 4.08 -> 2.97 s, fresh 8.11 -> 6.42 s. Per-torrent medians
are in the commit message of "Ask peers for metadata while the get_peers
lookup is still running".

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

A peer that has not answered the handshake within 1 s gives its slot to the
next one (at most 3x `max_peer_connections` attempts open), shared, builds
alternated base, new, base, new; desktop numbers are from the commit message
of "Give a peer's metadata slot to the next one when it is slow to answer".

| Where | calls per side | median s | p90 s | max s | mean s |
|---|---|---|---|---|---|
| desktop | 45 | 2.45 -> 2.30 | 12.12 -> 7.15 | 23.94 -> 11.84 | |
| Android emulator, emulated 3G (384 kbit/s, 35-200 ms), 4 slots | 30 | 2.54 -> 3.04 | 9.38 -> 4.53 | 41.23 -> 8.06 | 5.37 -> 3.42 |

    adb emu network delay umts; adb emu network speed umts
    adb shell 'cd /data/local/tmp && ./bench_metadata_base 5'   # aarch64 builds of the bench, pushed

Emulator raw seconds, per torrent, base then new:

    Sintel          15.15 2.87 2.85 2.54 9.12 6.07 1.53 2.16 1.66 3.63
                    6.48 3.23 7.37 4.20 3.68 3.42 3.28 3.30 3.67 3.39
    Big Buck Bunny  14.08 9.38 41.23 2.21 6.81 2.06 2.48 1.66 2.30 1.78
                    2.83 4.53 4.26 3.79 2.04 3.92 2.84 2.77 2.26 8.06
    Tears of Steel  2.55 1.61 1.98 1.02 1.12 2.36 7.44 5.87 2.66 2.87
                    2.85 2.09 2.27 2.36 2.33 2.46 2.54 2.14 1.93 2.23

The tail shrinks on both. On 3G the median rose 0.5 s, Sintel's most (2.86
-> 3.54): not separated from noise at this N. A cause it could have, not
checked: slow peers that answer late now fetch the metadata too, so more
copies of it share the 47 KB/s link at once.

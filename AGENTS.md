# omnid

One daemon that reaches the clearnet, Tor, I2P, Hyphanet and BitTorrent magnets, crawls them and
keeps one search index; also an Android app. `README.md` is the user manual; this file is for
agents changing the code. The eng kit's manual is `.agents/README.md`.

## Repository map

- `crates/omnid-core` — `Source` (address parsing, canonical form, link joining) and `IndexedDoc`.
- `crates/omnid-net` — `Gateway::fetch` / `Gateway::connect`: every outbound connection, the
  private-address guard, Tor (arti), TLS, decompression caps.
- `crates/omnid-dht` — read-only BitTorrent DHT client and its own bencode codec.
- `crates/omnid-index` — Tantivy schema, analyzer, ranking, snippets, mirror folding.
- `crates/omnid-crawl` — crawl store (SQLite), frontier, robots.txt, extraction, safety gate,
  crawl loop. End-to-end tests in `crates/omnid-crawl/tests/e2e.rs`.
- `crates/omnid` — the daemon as a library (`omnid::start`), the CLI, the HTTP API, the search page
  (`src/ui`, compiled in), the SOCKS proxy.
- `crates/omnid-android` + `android/` — JNI bridge and the Kotlin app (platform APIs only, plus
  GeckoView for the browser: `Browser.kt`, `BrowserActivity.kt`, `BrowserConfig.kt` and the
  extension in `android/app/src/main/assets/browser-proxy/`).

Dependencies point down this list; `omnid-core` depends on no other omnid crate.

## Authoritative commands

- Fast check: `./.agents/bin/eng verify --profile fast` (fmt, clippy `-D warnings`, tests)
- Full check: `./.agents/bin/eng verify --profile full`
- Targeted test: `cargo test -p <crate> [test name]`, e.g. `cargo test -p omnid-crawl --test e2e`
- Live tests against real routers: `scripts/live-routers.sh all` (starts i2pd and Hyphanet, runs every
  `#[ignore]`d live test, tears down); Tor-only: `cargo test --workspace -- --ignored live_tor`
- Benchmarks (record results in `docs/benchmarks.md`): `cargo run --release -p omnid-crawl --example
  bench_crawl`, `-p omnid-index --example bench_search`, `-p omnid-net --example bench_tor`,
  `-p omnid-dht --example soak -- <minutes>`
- Android unit tests: `(cd android && ./gradlew testDebugUnitTest)`
- Android: `android/build-rust.sh && (cd android && ./gradlew assembleRelease)`; needs the SDK,
  an NDK and the `aarch64-linux-android` target. Not part of the eng profiles.
- Security: `./.agents/bin/eng security`

## Architectural invariants

- Only `omnid-net` and `omnid-dht` open network connections. Clearnet connections go through the
  private-address guard unless `allow_private_addresses` is set.
- The app's browser (GeckoView) reaches the network only through the daemon's browser proxy
  (`socks::Access::Browser` → `Gateway::connect_browser`: password required, Tor or i2pd, never
  direct). Its Gecko prefs send anything else to a dead-end port and are read back before a page
  loads (`BrowserConfig.critical`); keep both when changing either side. Bump GeckoView with every
  Firefox release: it carries the security fixes.
- Media is never downloaded: bodies are read only for `is_indexable_type`, checked on headers
  before the body. The DHT fetches only info dicts over `ut_metadata`; never pieces.
- Everything from the network is hostile: no panics on malformed input, explicit caps on bytes,
  nesting and value counts (see `bencode::Limits`, `FetchOptions::max_bytes`, `http::decode`).
  New parsers come with a bound and a test that exercises it.
- HTML is tokenized, never built into a tree (nesting-depth DoS).
- The page renders crawled text as text only; the API checks `Host` on loopback and the token
  when set. Onion and `.i2p` addresses are never made clickable in an ordinary browser.
- Tests never touch the network; live tests are `#[ignore]`d.

## Public compatibility

- The index directory carries `omnid_index::SCHEMA_VERSION` (`index-v3`); a schema change bumps
  it instead of migrating in place, and `SearchIndex::migrate_from` copies the previous version's
  stored fields into the new one (`omnid::open_index`), so new fields need a default for old docs.
- `crawl.sqlite`'s `pages` table, the HTTP API (`/api/search`, `/api/submit`, `/api/crawl`,
  `/api/status`) and its JSON fields, CLI flags and the JNI symbols `Java_cc_wckd_omnid_Native_*`
  are contracts; change them explicitly and update `README.md`.

## Tracking

Plane workspace `wckd`; this repository is not in `WCKDai/.github/PLANE.md` yet, so it goes in the
Lab project until it is.

<img src="assets/icon.svg" width="64" height="64" alt="">

# omnid

One daemon that reaches the clearnet, Tor, I2P, Hyphanet and BitTorrent
magnets, crawls them politely, and keeps a single search index over all of
them. Content mirrored on several networks shows up as one result with every
route to it. It runs as a desktop/server daemon and as an Android app.

```
          ┌─────────── local API + search page ───────────┐   SOCKS5 proxy (opt-in)
          │                                                │          │
 seeds ─▶ crawl store (SQLite) ─▶ frontier ─▶ safety ─▶ robots.txt ─▶ fetch ─▶ extract ─▶ safety ─▶ index
              ▲                                                       │                          (Tantivy)
              └──────────────── discovered links, revisits ◀──────────┤
                                                                      ▼
                            Gateway::fetch / Gateway::connect route by network
      clearnet: reqwest (or Tor exits) │ onion: arti, in-process │ i2p: i2pd proxy │ hyphanet: FProxy
```

## Status

| Piece | State |
|---|---|
| Address parsing: URLs, bare hosts, v3 onions, `.i2p`, `freenet:` keys, magnets | done, unit tested |
| Clearnet fetch, gzip/brotli/deflate | done, live tested |
| Tor fetch, HTTP and HTTPS, via embedded arti, gzip over Tor | done, live tested against real onion services |
| I2P fetch via i2pd's HTTP proxy | written; tested against a stand-in, not a live router |
| Hyphanet fetch via FProxy | written; not tested against a live node |
| Magnets | harvested from pages and indexed by info hash; metadata needs the DHT module |
| Index: stemming, accent folding, snippets, ranking, facets, paging, mirror folding | done, unit tested |
| Crawler: persistent store, revisits, backoff, politeness, robots.txt, pause/resume | done, end-to-end tested |
| Charset detection (BOM, header, `<meta>`) | done, unit tested |
| Safety gate | done for text; see [Safety](#safety) |
| Local API and search page, optional token | done, live tested |
| SOCKS5 proxy (`.onion`/`.i2p` for any browser) | done, live tested with curl over Tor |
| Android app (arm64) | done, run on an Android 16 emulator |

Not built yet: the DHT module (BEP 51 harvesting, BEP 9 metadata), router
lifecycle management (starting i2pd and Hyphanet as child processes), index
sync between nodes, near-duplicate detection, a desktop app shell.

## Quick start

```sh
cargo build --release
B=target/release/omnid

$B fetch https://example.com/              # one fetch, routed by network
$B fetch http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
$B crawl --max-pages 50 https://example.com/   # crawl until nothing is due, then exit
$B crawl                                   # resume whatever the store has due
$B search "tor browser" -n onion
$B serve --seeds seeds.txt                 # daemon + http://127.0.0.1:7171
$B serve --socks 127.0.0.1:7172            # ...plus a SOCKS5 proxy
curl --socks5-hostname 127.0.0.1:7172 http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
```

Tor starts with the daemon and needs nothing installed. For I2P run i2pd
(HTTP proxy on 4444); for Hyphanet run a node (FProxy on 8888). `--no-tor`,
`--i2p-proxy` and `--fproxy` change that; `--clearnet-via-tor` sends
clearnet traffic through Tor exits. Data lives in the platform data
directory (`--data-dir` to override): the index in `index-v2/`, the crawl
store in `crawl.sqlite`, Tor state in `tor/`.

Search syntax: words (all required, falling back to any word when nothing has
all of them), `"a phrase"`, `-exclude`, `title:word`, `site:host`,
`network:onion`.

### API

| | |
|---|---|
| `GET /api/search?q=…&network=onion,i2p&offset=0&limit=20` | hits with marked snippets, `total`, `network_counts`, `relaxed` |
| `POST /api/submit` with `{"url": "…"}` | crawl an address now, even if known |
| `POST /api/crawl` with `{"paused": true}` | pause or resume crawling |
| `GET /api/status` | router states, index size, crawl counters, pause reason |

`--api-token` (or `OMNID_API_TOKEN`) makes every request need
`Authorization: Bearer <token>` or the `omnid_token` cookie.

### Android

```sh
android/build-rust.sh                # cross-compile crates/omnid-android with the NDK
cd android && ./gradlew assembleRelease
adb install app/build/outputs/apk/release/app-release.apk
```

Needs the Android SDK with an NDK, JDK 17 and the `aarch64-linux-android` Rust
target. Without `android/keystore.properties` the release APK is signed with
the debug key: fine for sideloading, not for distribution. The app runs the
same daemon in a foreground service, shows its search page in a WebView, and
crawls only on a charger and an unmetered network unless "Crawl on battery
and mobile data" is on. Share an onion, magnet or link to the app to crawl
it. A new install seeds itself with the Tor Project's and the BBC's onion
services.

## Layout

| Path | Owns |
|---|---|
| `crates/omnid-core` | `Source` (canonical address + network), `IndexedDoc` |
| `crates/omnid-net` | `Gateway::fetch` and `Gateway::connect`: transports, redirects, the private-address guard |
| `crates/omnid-index` | Tantivy schema, analyzer, ranking, snippets, mirror folding |
| `crates/omnid-crawl` | crawl store, frontier, robots.txt, extraction, safety gate, crawl loop |
| `crates/omnid` | the daemon as a library (`omnid::start`) and the CLI; API, search page, SOCKS proxy |
| `crates/omnid-android` | JNI bridge for the app |
| `android/` | the Android app (Kotlin, platform APIs only) |

## Design notes

- **Redirects are resolved against the source that issued them.** A clearnet
  page redirecting to an onion is followed over Tor. The crawler takes
  redirects unfollowed and queues the target, so it is deduplicated,
  rate-limited and checked against the link policy like any link.
- **The crawl survives restarts.** Every known address lives in SQLite with
  its next visit, consecutive failures and liveness; the in-memory frontier is
  a bounded working set pulled from it, shallowest first. Pages are revisited
  after 3 days (onion, I2P, magnets) or 7 (clearnet, Hyphanet), failures back
  off from an hour to 30 days, 404/410 leave the index, and a router that is
  not running only defers its sites by 10 minutes.
- **Ranking** multiplies BM25 by provenance and reliability:
  `(0.5 + trust) × (0.6 + 0.4 × liveness)`. Operator seeds have trust 0.8,
  discovered links 0.5; liveness is a moving average of whether the address
  answered. Text relevance still dominates.
- **Mirror folding.** `content_hash` is BLAKE3 over the page's normalized
  visible text, so copies that differ only in markup or link targets share
  it. Matching is exact; near-duplicates need a similarity hash later.
- **Politeness.** Per network: clearnet 32 concurrent with 1 s per site,
  onion 16 with 5 s, I2P 4 with 5 s, Hyphanet 2 with 2 s (the phone uses
  6/6/2/1). One request per site at a time. robots.txt is honored and fetched
  over the same network as the page.
- **Link following.** Overlay and magnet links are always harvested.
  Clearnet links are followed only from clearnet pages, and by default only
  within the same site (`www.` included; `--follow-offsite` lifts that), so a
  seed list of directory pages does not turn into a crawl of the web.
- **HTML is tokenized, not parsed into a tree.** A tree builder does work
  proportional to nesting depth on every element: a 600 KB page of nested
  `<div>`s took 11.5 s in release. The tokenizer is linear.
- **Index format is versioned** by directory name (`index-v2`). A schema
  change opens a new index rather than misreading an old one.

## Safety

- **Media is never downloaded.** Only `text/html`, `application/xhtml+xml`
  and `text/plain` bodies are read; the check runs on the response headers
  before the body is fetched. A missing Content-Type is refused.
- **Denylist** (`--denylist`): hostnames (subdomains included), Hyphanet keys,
  magnets, or MD5 hex digests of hostnames, the form published blocklists use.
  Denied sites are never fetched and links to them are never queued.
- **Blocked terms** (`--blocked-terms`): a matching page is not indexed, its
  links are not followed, it is never revisited, and it is removed from the
  index if it was there before the list changed.
- On Android, `denylist.txt` and `blocked-terms.txt` in the app's data
  directory are loaded the same way.
- **Known-abuse media hashes are not checked yet.** PhotoDNA, NCMEC and IWF
  lists are licensed to vetted operators and cannot ship with the code. That
  check has to exist in `omnid-crawl/src/safety.rs` before any media handling
  is added. Operators of a public index should also take legal advice on
  their reporting obligations.

The daemon itself:

- Clearnet fetches and proxied connections refuse loopback, private,
  link-local, CGNAT and other non-public addresses, checked after DNS
  resolution so rebinding cannot get around it. `--allow-private-addresses`
  turns this off.
- Compressed responses decode under the same size cap as plain ones, so a
  compression bomb stays small.
- The API binds to loopback and rejects requests whose `Host` is not
  localhost (DNS rebinding). Submissions must be JSON, which a cross-site
  page cannot send without a CORS preflight that is never granted. An API
  token keeps out other local users and, on a phone, other apps; the Android
  app sets a fresh random one each start.
- The search page renders crawled text as text only, sends a strict CSP, and
  links only clearnet and magnet routes. Onion and `.i2p` addresses are shown
  for copying, because opening them in an ordinary browser leaks a DNS lookup.
- The SOCKS proxy has no authentication; it is off unless `--socks` is given
  and should stay on loopback.

## Known limitations

- HTTPS onions need a certificate that validates; expired or self-signed ones
  are refused. Facebook's onion rejects the TLS handshake (HTTPS onions such
  as BBC's work).
- The SOCKS proxy does not isolate streams per client the way Tor Browser
  does; everything shares circuits.
- The Android build is arm64 only.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

No test touches the network: crawls run against local servers, the proxy and
tunnels against local stand-ins, and the Android bridge without Tor.

## License

MIT, see [LICENSE](LICENSE). Dependencies are permissively licensed (MIT,
Apache-2.0, BSD, ISC, Zlib, Unicode) except `option-ext`, an unmodified
MPL-2.0 dependency of arti; binary distributions, the APK included, should
ship the dependencies' license notices.

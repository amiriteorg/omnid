<img src="crates/omnid/src/ui/icon.png" width="64" height="64" alt="">

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
| I2P via i2pd: fetching, crawling and the SOCKS tunnel | done, live tested against i2pd 3.0 (stats.i2p, reg.i2p, notbob.i2p) |
| Hyphanet via FProxy: fetching, crawling, newer-edition redirects | done, live tested against a Hyphanet 1507 node |
| Magnets: names, file lists and swarm size from the DHT (BEP 5/9), never content | done, live tested; 30-minute soak: 30/30 lookups, flat 11–12 MB |
| DHT harvesting (BEP 51), opt-in | done, live tested (about 1,700 info hashes in 30 s) |
| Index: stemming, accent folding, snippets, ranking, facets, paging, mirror folding | done, unit tested |
| Crawler: persistent store, revisits, backoff, politeness, robots.txt, pause/resume | done, end-to-end tested |
| Charset detection (BOM, header, `<meta>`) | done, unit tested |
| Safety gate | done for text; see [Safety](#safety) |
| Local API and search page, optional token | done, live tested |
| SOCKS5 proxy (`.onion`/`.i2p` for any browser) | done, live tested with curl over Tor |
| Android app (arm64) | done, run on an Android 16 emulator: crawled 225 onion pages over embedded Tor, magnets via the DHT |
| Browser in the Android app: GeckoView, private, every request through the daemon | done, run on the emulator: onion and clearnet pages over Tor, check.torproject.org reports Tor, New identity changes the exit, reg.i2p through the i2pd app |

Not built yet: router lifecycle management (starting i2pd and Hyphanet as child processes), index
sync between nodes, near-duplicate detection, a desktop app shell.

## Quick start

```sh
cargo build --release
B=target/release/omnid

$B fetch https://example.com/              # one fetch, routed by network
$B fetch http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
$B fetch 'magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10'   # Sintel: name, files, peers
$B crawl --max-pages 50 https://example.com/   # crawl until nothing is due, then exit
$B crawl                                   # resume whatever the store has due
$B search "tor browser" -n onion
$B serve --seeds seeds.txt                 # daemon + http://127.0.0.1:7171
$B serve --socks 127.0.0.1:7172            # ...plus a SOCKS5 proxy
curl --socks5-hostname 127.0.0.1:7172 http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
curl --socks5-hostname 127.0.0.1:7172 http://hyphanet/USK@…/   # a freesite, through the Hyphanet node
```

Tor starts with the daemon and needs nothing installed. For I2P run i2pd
(HTTP proxy on 4444); for Hyphanet run a node (FProxy on 8888). Pages on a
network whose router is down are put off rather than failed, and crawled as
soon as it is back (omnid looks every 30 seconds). `--no-tor`,
`--i2p-proxy` and `--fproxy` change that; `--clearnet-via-tor` sends
clearnet traffic through Tor exits. omnid joins the BitTorrent DHT as a
read-only node to resolve magnets (`--no-dht` to stay out); `--dht-harvest`
also indexes info hashes sampled from the whole DHT. Data lives in the platform data
directory (`--data-dir` to override): the index in `index-v3/`, the crawl
store in `crawl.sqlite`, Tor state in `tor/`.

A Hyphanet node can also run on another machine, such as an always-on
computer that a phone reaches over Tailscale; point omnid at it with
`--fproxy http://<address>:8888` (in the app, ⋮ → Hyphanet node). FProxy has
no passwords, so in the node's `freenet.ini` bind it to loopback and that one
private address, let in only your own devices, and keep its settings,
downloads to disk and FCP for the machine itself:

```ini
fproxy.bindTo=127.0.0.1,<the machine's Tailscale address>
fproxy.allowedHosts=127.0.0.1,0:0:0:0:0:0:0:1,<each client's address>
fproxy.allowedHostsFullAccess=127.0.0.1,0:0:0:0:0:0:0:1
fproxy.publicGatewayMode=true
fcp.bindTo=127.0.0.1
fcp.allowedHosts=127.0.0.1
```

`allowedHostsFullAccess` only takes effect with `publicGatewayMode`. Do not
put FProxy behind `tailscale serve` or another local proxy: the node would see
every client as 127.0.0.1 and give each one full access.

Search syntax: words (all required, falling back to any word when nothing has
all of them), `"a phrase"`, `-exclude`, `title:word`, `site:host`,
`network:onion`. The last word also matches the words it starts, from three
letters, so results come while it is being typed. When nothing matches, plain
words are matched with a letter wrong, missing, extra or swapped (two from
eight letters), and the results say so (`fuzzy`). On the search page, an
address typed into the search field (an onion or `.i2p` host, a URL, a magnet
or a Hyphanet key) can be opened or added to the index right there.

### API

| | |
|---|---|
| `GET /api/search?q=…&network=onion,i2p&offset=0&limit=20&lang=de,en` | hits with marked snippets, `lang`, `total`, `network_counts`, `relaxed`, `fuzzy` |
| `POST /api/submit` with `{"url": "…"}` | crawl an address now, even if known |
| `POST /api/crawl` with `{"paused": true}` | pause or resume crawling |
| `GET /api/status` | router states, index size, crawl counters, pause reason |
| `GET /hyphanet/<key>` | a redirect to that Hyphanet key on the node's FProxy (keys only); the search page's Hyphanet links |

`--api-token` (or `OMNID_API_TOKEN`) makes every request need
`Authorization: Bearer <token>` or the `omnid_token` cookie.

### Android

```sh
android/build-rust.sh                # cross-compile crates/omnid-android with the NDK
cd android && ./gradlew assembleRelease
adb install app/build/outputs/apk/release/app-release.apk
```

`scripts/android-smoke.sh [--fresh] [APK]` installs the build on a headless
emulator it creates (a Pixel 7 on Android 16, arm64, with a 16 GB data partition) and checks that
the daemon starts, Tor bootstraps, the search page and the name show, the
notification follows the charger, and nothing crashed; `scripts/android-smoke.sh
stop` shuts the emulator down.

Needs the Android SDK with an NDK, JDK 17 and the `aarch64-linux-android` Rust
target. Release builds are signed with the key named in
`android/keystore.properties` (git-ignored; `storeFile`, `storePassword`,
`keyAlias`, `keyPassword`), with APK signature scheme v3, which allows the key to
be rotated later (every supported Android version verifies v3); without that
file they fall back to the debug key, which is fine for sideloading only.
Every update must be signed with the same key, so back the keystore up. The app runs the
same daemon in a foreground service, shows its search page in a WebView, and
crawls only on a charger and an unmetered network unless "Crawl on battery
and mobile data" is on. Share an onion, magnet or link to the app to crawl
it. A new install seeds itself with the Tor Project's and the BBC's onion
services, and with I2P's hubs (the I2P Project, I2P Planet and the reg.i2p,
inr.i2p and stats.i2p registries), which wait until an I2P router runs; from
them the crawler follows links across I2P. Magnets are resolved through the
DHT on the phone too; the phone never harvests the DHT. For `.i2p`, install the [i2pd app](https://f-droid.org/packages/org.purplei2p.i2pd/)
from F-Droid and start its router: its HTTP proxy is on 127.0.0.1:4444, where
omnid looks, so there is nothing to set up. Until then the I2P row under the
network strip offers "Get i2pd" or "Open i2pd". For Hyphanet, ⋮ → Hyphanet node
takes a node elsewhere, such as your computer's over Tailscale (see above);
empty means one on the phone at 127.0.0.1:8888.

Results open in omnid's own browser, GeckoView (Firefox's engine) in private
mode: onion and `.i2p` results as well as clearnet ones, typed addresses too,
and Hyphanet pages through your node, as `http://hyphanet/USK@…` (a key typed
into the address bar opens there too). Every request goes through the daemon,
over Tor (or i2pd for `.i2p`, the Hyphanet node for `hyphanet`), with
each site on Tor circuits of its own; ⋮ → New identity drops the session and
gives every site new circuits. GeckoView makes the APK much larger; see
[Known limitations](#known-limitations).

## Layout

| Path | Owns |
|---|---|
| `crates/omnid-core` | `Source` (canonical address + network), `IndexedDoc` |
| `crates/omnid-net` | `Gateway::fetch` and `Gateway::connect`: transports, redirects, the private-address guard |
| `crates/omnid-index` | Tantivy schema, analyzer, ranking, snippets, mirror folding |
| `crates/omnid-crawl` | crawl store, frontier, robots.txt, extraction, safety gate, crawl loop |
| `crates/omnid-dht` | read-only DHT client: get_peers, BEP 9 metadata, BEP 51 sampling, own bencode |
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
- **Magnets** are resolved by a read-only DHT node (BEP 43 `ro`, rate
  limited, private and low-port peers skipped). Only the info dictionary is
  fetched over `ut_metadata` and checked against the info hash; no piece is
  ever requested or uploaded. Liveness is the peers found over 20, capped at 1.
- **Pages in the reader's language rank first.** A page's language is the
  one its `<html lang>` declares, or else what its text looks like. Searches
  prefer the languages in `lang=` or the browser's `Accept-Language` (the CLI:
  `--lang`, else `$LANG`); pages in other known languages score lower, and
  pages nearer a site's root a little higher.
- **Index format is versioned** by directory name (`index-v3`). A schema
  change builds a new index rather than misreading an old one; the daemon
  copies the previous version's pages into it on first start, then removes
  the old directory.

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
- **DHT harvesting is opt-in.** It indexes the names and file lists of
  torrents nobody linked to, whatever they are, so run it only with a
  blocked-terms list, which is checked against names and every file path.
  The Android app never harvests.
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
  for copying, because opening them in an ordinary browser leaks a DNS lookup;
  inside the Android app they open in its own browser.
- The SOCKS proxy has no authentication; it is off unless `--socks` is given
  and should stay on loopback.

The Android app's browser:

- **Nothing leaves it directly.** A bundled extension sends every request to
  a SOCKS5 listener in the daemon that wants the app's per-start secret, so
  other apps on the phone cannot use it, and that listener only goes through
  Tor or i2pd, or to the Hyphanet node you set for the reserved name
  `hyphanet`, never direct anywhere else, whatever the crawler is set to.
  A page cannot load anything from I2P or Hyphanet unless it is there itself,
  so no site can probe whether the phone runs a router or node. Requests the
  extension does not route (Gecko's own) go to port 9 on loopback, which no
  app can listen on, and fail. Names resolve at the exit, never on the phone;
  localhost goes through the proxy too, so pages cannot reach omnid's API.
- **It checks before it loads.** GeckoView reads the proxy, DNS and WebRTC
  prefs from a file at startup and ignores one it cannot read, so the app
  reads the critical ones back and loads nothing unless they took effect.
- **Each site on its own circuits**, as in Tor Browser: the site you are on is
  the proxy username, and the daemon keeps a Tor isolation token per site. A
  stream that fails is tried once more on a new circuit for that site.
- Private mode only: cookies and storage last until the browser closes.
  Firefox's resist-fingerprinting is on, WebRTC, WebGL, geolocation and
  prefetching are off, HTTPS only on the clearnet (onions are exempt, and a
  `.i2p` site without HTTPS is reloaded over HTTP: I2P, like Tor for onions,
  encrypts the whole way), strict tracking protection, no Safe Browsing lookups. Site permissions (camera,
  microphone, location, notifications) are refused, downloads are off, and
  screenshots and the recent-apps thumbnail are blocked unless allowed in ⋮.

## Known limitations

- HTTPS onions need a certificate that validates; expired or self-signed ones
  are refused. Facebook's onion rejects the TLS handshake (HTTPS onions such
  as BBC's work).
- The `--socks` proxy does not isolate streams per client the way Tor Browser
  does; everything shares circuits. (The Android browser's proxy does.)
- The Android browser is not Tor Browser. It runs release GeckoView with
  Firefox's own fingerprinting protection, not Tor Browser's patched ESR, so a
  site can tell it apart from Tor Browser and from other omnid users only
  as far as resist-fingerprinting allows. Use Tor Browser where that matters.
  GeckoView must be updated with every Firefox release, which carries its
  security fixes (`android/app/build.gradle.kts`).
- On the phone, `.i2p` needs the i2pd app with its router running; omnid
  cannot start it (i2pd offers other apps no way to).
- GeckoView takes the APK from 24 MB to 103 MB, with native libraries
  compressed in the APK and unpacked on install.
- Through the SOCKS proxy, `.i2p` requests reach the site with your client's
  own headers: i2pd rewrites them only on its plain HTTP proxy. Some eepsites
  (stats.i2p) refuse anything but I2P's standard `User-Agent: MYOB/6.66 (AN/ON)`.
  The crawler uses the plain proxy and is not affected.
- Dependency advisories (`eng security`, osv-scanner): the bundled SQLite in
  `libsqlite3-sys` 0.38 has a local-only CVE fixed in 0.39, which waits on arti
  (0.48 pins rusqlite 0.40; one SQLite per build) and needs crafted files in
  omnid's own data directory. `rsa` and `crypto-bigint` timing issues come
  through arti's key management and concern private-key operations a Tor
  client does not perform. bincode, chrono, humantime and paste are flagged
  as unmaintained; bincode, chrono and crypto-bigint are in `Cargo.lock` but
  compiled into no omnid build. Each is a reviewed exception in
  `osv-scanner.toml` that expires on 2027-02-21.
- The Android build is arm64 only.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

No test touches the network unless it is ignored: crawls run against local
servers, the proxy and tunnels against local stand-ins, and the Android bridge
without Tor. Property tests (proptest) throw generated input at the parsers.
The eng kit wraps the rest: `./.agents/bin/eng verify --profile fast` and
`./.agents/bin/eng security`. Android: `cd android && ./gradlew testDebugUnitTest`.

Live tests run against real networks and routers:

```sh
scripts/live-routers.sh all     # start i2pd and Hyphanet, run every live test, tear down
cargo test --workspace -- --ignored live_tor   # just the Tor ones; needs no router
```

The script starts a client-only i2pd (`--notransit`) and a temporary Hyphanet
node from a package it checks against a pinned hash and the release key, waits
until each serves pages, and deletes their state afterwards. A live test whose
router is not running fails and says so.

Benchmarks and their recorded results are in [docs/benchmarks.md](docs/benchmarks.md):
`bench_crawl`, `bench_search`, `bench_tor` and the DHT `soak`.

## License

MIT, see [LICENSE](LICENSE). Dependencies are permissively licensed (MIT,
Apache-2.0, BSD, ISC, Zlib, Unicode) except `option-ext`, an unmodified
MPL-2.0 dependency of arti, and GeckoView in the Android app, MPL-2.0 and
unmodified (its licenses are at `about:license` in the browser); binary
distributions, the APK included, should ship the dependencies' license
notices.

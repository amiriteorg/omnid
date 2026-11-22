<img src="crates/omnid/src/ui/icon.png" width="64" height="64" alt="">

# omnid

One daemon that reaches the clearnet, Tor, I2P, Hyphanet, IPFS, Nostr and
BitTorrent magnets, crawls them politely, and keeps a single search index over all of
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
                         ipfs: an IPFS gateway (kubo's) │ nostr: relays, over WebSocket
```

## Status

| Piece | State |
|---|---|
| Address parsing: URLs, bare hosts, v3 onions, `.i2p`, `freenet:` keys, `ipfs://`/`ipns://` (a CID in any base is one address), `nostr:` entities (NIP-19), magnets | done, unit and property tested |
| Clearnet fetch, gzip/brotli/deflate | done, live tested |
| Tor fetch, HTTP and HTTPS, via embedded arti, gzip over Tor | done, live tested against real onion services |
| I2P via i2pd: fetching, crawling and the SOCKS tunnel | done, live tested against i2pd 3.0 (stats.i2p, reg.i2p, notbob.i2p) |
| Hyphanet via FProxy: fetching, crawling, newer-edition redirects | done, live tested against a Hyphanet 1507 node |
| IPFS via a gateway: CIDs, IPNS keys and DNSLink names, links through public gateways folded into the content | done, live tested against kubo 0.43.1 (docs.ipfs.tech, Wikipedia on IPFS); not in the app yet |
| Nostr from relays: notes, articles, profiles and public chat channels, every event's ID and signature checked | done, live tested against nos.lol, Primal and nostr.mom; not in the app yet |
| Magnets: names, file lists and swarm size from the DHT (BEP 5/9), never content | done, live tested; 30-minute soak: 30/30 lookups, flat 11–12 MB |
| DHT harvesting (BEP 51), opt-in | done, live tested (about 1,700 info hashes in 30 s) |
| Index: stemming, accent folding, snippets, ranking, facets, paging, mirror folding | done, unit tested |
| Crawler: persistent store, revisits, backoff, politeness, robots.txt, pause/resume | done, end-to-end tested |
| Charset detection (BOM, header, `<meta>`) | done, unit tested |
| Safety gate | done for text; see [Safety](#safety) |
| Local API and search page, optional token | done, live tested |
| Proxy, SOCKS5 and HTTP (`.onion`/`.i2p` for any browser or app), open or private | done, live tested with curl over Tor; the app's proxy for other apps run on the emulator (Tor exits per username, an onion, reg.i2p) |
| Android app (arm64) | done, run on an Android 16 emulator: crawled 225 onion pages over embedded Tor, magnets via the DHT |
| Browser in the Android app: GeckoView, private, every request through the daemon | done, run on the emulator: onion and clearnet pages over Tor, check.torproject.org reports Tor, New identity changes the exit, reg.i2p through the i2pd app |
| An I2P identity per browser site, through the router's SAM bridge | done, tested against a stand-in bridge and run against i2pd on a computer over Tailscale; not yet with the i2pd app's own SAM bridge |
| Downloads in parallel byte ranges, each connection on Tor circuits of its own | done, tested against local servers; measured over Tor in [docs/benchmarks.md](docs/benchmarks.md) |
| The phone's crawler through Tor, torrent lookups off by default | done, run on the emulator, upgrading an install that crawled directly |

Every release so far has run on an emulator and on macOS, not yet on a physical phone. What
comes next, and what is not built yet, is in [ROADMAP.md](ROADMAP.md).

## Quick start

```sh
cargo build --release
B=target/release/omnid

$B fetch https://example.com/              # one fetch, routed by network
$B fetch http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
$B fetch 'magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10'   # Sintel: name, files, peers
$B download https://www.python.org/ftp/python/3.12.0/Python-3.12.0.tar.xz   # in parallel byte ranges, saved here
$B download --clearnet-via-tor --connections 8 -o ~/Downloads/ https://…  # through Tor, eight circuits at once
$B crawl --max-pages 50 https://example.com/   # crawl until nothing is due, then exit
$B crawl                                   # resume whatever the store has due (a new store: the default seeds)
$B search "tor browser" -n onion
$B serve --seeds seeds.txt                 # daemon + http://127.0.0.1:7171
$B serve --socks 127.0.0.1:7172            # ...plus a proxy, SOCKS5 and HTTP on one port
curl --socks5-hostname 127.0.0.1:7172 http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
curl --socks5-hostname 127.0.0.1:7172 http://hyphanet/USK@…/   # a freesite, through the Hyphanet node
curl -x http://127.0.0.1:7172 http://reg.i2p/                  # the same port as an HTTP proxy
OMNID_SOCKS_PASSWORD=… $B serve --socks 127.0.0.1:7172         # a private proxy (below)
curl -x http://myapp:…@127.0.0.1:7172 https://check.torproject.org/api/ip
```

The proxy speaks SOCKS5 and HTTP on the same port: HTTP CONNECT tunnels, and
an absolute-form request (`GET http://site/`) is passed on in origin form, one
per connection. With `--socks-password` (or `OMNID_SOCKS_PASSWORD`) it is
private: only clients that send the password, with any username, as SOCKS5
credentials or HTTP Basic, get in; nothing goes direct (the clearnet through
Tor exits, `.i2p` through i2pd, `hyphanet` to the node); and each username gets
Tor circuits of its own, so apps given different usernames cannot be linked
by circuit. Either way a client that says nothing for 15 seconds is dropped,
and one listener serves at most 512 clients at once.

Tor starts with the daemon and needs nothing installed. For I2P run i2pd
(HTTP proxy on 4444); for Hyphanet run a node (FProxy on 8888); for IPFS run
an IPFS node such as kubo (its gateway on 8080). Pages on a
network whose router is down are put off rather than failed, and crawled as
soon as it is back (omnid looks every 30 seconds). `--no-tor`,
`--i2p-proxy`, `--fproxy` and `--ipfs-gateway` change that; `--clearnet-via-tor` sends
clearnet traffic through Tor exits. Each takes several routers, repeated or
separated by commas, best first (`--i2p-proxy http://100.x.y.z:4444,http://127.0.0.1:4444`):
each reaches the whole network, so a request goes to the first that answers.
One that cannot be reached rests (15 seconds, doubling up to ten minutes) and
is tried last until it is back; one that answers with a gateway error (5xx,
as i2pd does when it cannot reach a site) hands the request to the next. The
browser's and the SOCKS proxy's streams also start on the next router when
the first has not answered within 3 seconds, and take whichever connects
first. Where a router has SAM on (port 7656 on its host by default;
`--i2p-sam sam://host:port` to name others, `--no-i2p-sam` for none), the app's
browser and the password proxy reach I2P through it instead, each site from
an I2P identity of its own (see Anonymity). omnid joins the BitTorrent DHT as a
read-only node to resolve magnets (`--no-dht` to stay out); `--dht-harvest`
also indexes info hashes sampled from the whole DHT. Data lives in the platform data
directory (`--data-dir` to override): the index in `index-v3/`, the crawl
store in `crawl.sqlite`, Tor state in `tor/`.

A Hyphanet node can also run on another machine, such as an always-on
computer that a phone reaches over Tailscale; point omnid at it with
`--fproxy http://<address>:8888` (in the app, ⋮ → Settings → Hyphanet node). FProxy has
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

IPFS content is fetched from a path gateway as `/ipfs/<CID>/…` and
`/ipns/<name>/…`: a CID is asked for as CIDv1, whatever base it was written
in, so `Qm…` and `bafy…` are one address, and a link through a public gateway
(`https://ipfs.io/ipfs/…`, `https://<CID>.ipfs.dweb.link/…`) is the content
itself, fetched from your gateway. A page without a gateway's headers
(`Ipfs-Uri`, `X-Ipfs-Roots`, or the older `X-Ipfs-Path`) is refused, so a
development server that happens to be on 8080 is not indexed as IPFS. IPFS is
not anonymous: the node asks its peers for content from its own address, so
the crawler never follows a link into IPFS from an onion, I2P or Hyphanet page
(or from the clearnet with `--clearnet-via-tor`), where the page's author could
plant a CID and watch who asks for it. robots.txt is not asked for on IPFS.

Nostr comes from relays (`--nostr-relay`, by default nos.lol, Primal,
nostr.mom and Damus, four asked at once; `--no-nostr` for none), over
WebSocket. A `nostr:` address (NIP-19's `npub1…`, `note1…`, `naddr1…`, and
`nevent1…`/`nprofile1…`, whose relay hints are asked first) is a profile, an
event or an article; a public chat channel (NIP-28) is indexed with its
latest 50 messages, and a profile with links to its latest 20 notes, articles
and channels. No event is taken before its ID is checked against its content
and its signature against its author (BIP-340), so a relay can withhold an
event but not forge or alter one. Relays are reached as the clearnet is,
directly or through Tor exits with `--clearnet-via-tor`, so a page reached
anonymously leads the crawler to Nostr only through Tor. A `nostr:` link on a
web page, and pages of web clients that show one entity (njump.me,
primal.net and the like), are followed into Nostr; links in a note to the web
are not.

An I2P router can run elsewhere in the same way: `--i2p-proxy
http://<address>:4444` (in the app, ⋮ → Settings → I2P router). A router or
node at a Tailscale address (100.64.0.0/10) is asked only while this machine
has such an address on a tunnel interface: the range is also carrier-grade
NAT's, and with Tailscale off, plain HTTP to it could reach another
subscriber's device. i2pd's HTTP proxy binds
one address and has no access list, so keep it on loopback and put a relay that
admits only your devices' addresses on the private one; Tailscale ties each
peer's address to its key, so the address is who is connecting. Leave
`httpproxy.outproxy` unset, so the proxy reaches `.i2p` sites and nothing
else. Turn SAM on (`[sam]` with `enabled = true` and `address = 127.0.0.1`)
and relay its port, 7656, the same way, for the browser's per-site identities;
anyone who reaches SAM can make destinations on the router, so only your
devices. A relay that closes quiet connections is fine: omnid pings each
session every two minutes. Turn off what omnid does not use (`socksproxy`,
`bob`, `i2cp`, `i2pcontrol`). A participating router (no `notransit`) keeps
its own tunnels healthier than a client-only one.

Search syntax: words (all required, falling back to any word when nothing has
all of them), `"a phrase"`, `-exclude`, `title:word`, `site:host`,
`network:onion`. The last word also matches the words it starts, from three
letters, so results come while it is being typed. When nothing matches, plain
words are matched with a letter wrong, missing, extra or swapped (two from
eight letters), and the results say so (`fuzzy`). On the search page, an
address typed into the search field (an onion or `.i2p` host, a URL, a magnet,
a Hyphanet key, an IPFS address or a Nostr entity) can be opened or added to the index right there.

### API

| | |
|---|---|
| `GET /api/search?q=…&network=onion,i2p&offset=0&limit=20&lang=de,en` | hits with marked snippets, `lang`, `total`, `network_counts`, `relaxed`, `fuzzy` |
| `POST /api/submit` with `{"url": "…"}` | crawl an address now, even if known |
| `POST /api/crawl` with `{"paused": true}` | pause or resume crawling |
| `GET /api/status` | router states, index size, crawl counters, pause reason |
| `GET /hyphanet/<key>` | a redirect to that Hyphanet key on the node's FProxy (keys only); the search page's Hyphanet links |
| `GET /ipfs/<CID>/…`, `GET /ipns/<name>/…` | a redirect to that content on the IPFS gateway (CIDs and names only); the search page's IPFS links |

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
it. The app starts from the default seeds (below), and adds any it is
missing at every start, so an older install gains new ones without losing its
index. On the phone, magnets are looked up on the DHT only while Settings → Torrent lookups is
on (off by default; see Anonymity), and the phone never harvests the DHT. For `.i2p`, install the [i2pd app](https://f-droid.org/packages/org.purplei2p.i2pd/)
from F-Droid and start its router: its HTTP proxy is on 127.0.0.1:4444, where
omnid looks, so there is nothing to set up. Until then the I2P row under the
network strip offers "Get i2pd" or "Open i2pd". Turn on SAM in its `i2pd.conf`
(`enabled = true` under `[sam]`) for an I2P identity per site (see
Anonymity); while every site shares one, the I2P row offers Settings → I2P
router. ⋮ → Settings → I2P router takes
a router elsewhere, such as your computer's over Tailscale (see above), asked
before the i2pd app, which stays the fallback. For Hyphanet, ⋮ → Settings →
Hyphanet node takes a node elsewhere in the same way; empty means one on the
phone at 127.0.0.1:8888, which is also the fallback.

⋮ → Settings (also in the browser's ⋮) holds the rest, saved as they change:

- **Appearance**: the theme (as the phone, light or dark) for OMNId's screens
  and the search page, and the text size on web pages (85 to 150%).
- **Browser**: the security level (below), whether pages load images (off,
  pages load far faster over Tor), whether screenshots may show the
  browser, and how many connections a download uses.
- **Search**: the networks a search starts with, the languages to rank first
  (`de, en`; empty for the phone's own), and two lists the crawler reads at
  start: blocked words (a page with one is not indexed and its links are not
  followed) and blocked sites (never crawled), kept as `blocked-terms.txt` and
  `denylist.txt` in the app's data directory.
- **Crawling**: crawl speed (Gentle halves the connections per network, Fast
  doubles them; each site still gets one request at a time with its usual
  pause), "Crawl on battery and mobile data", and Ahmia's blocklist (on by
  default; see Safety; `--no-blocklist` on a computer).
- **Anonymity**: "Crawl the web through Tor" (on by default) and "Torrent
  lookups" (off by default); see Anonymity.
- **Networks**: the I2P router and Hyphanet node elsewhere.
- **Proxy for other apps**: other apps on the phone reach Tor, I2P and Hyphanet
  through OMNId at 127.0.0.1:7172, as SOCKS5 or HTTP, the private proxy above:
  never direct, each username on Tor circuits of its own, apart from the
  browser's. It wants its password (made on the phone, shown there to copy,
  and replaceable) unless "Require the password" is off, which lets any app on
  the phone use it. If another app holds the port, the row says so and the
  rest of OMNId runs without it.
- **About**: the version, the source code and the licenses.

Settings the crawler reads at start (crawl speed, the lists, the routers)
restart it; the browser takes its settings when it next comes to the front.

Results open in omnid's own browser, GeckoView (Firefox's engine) in private
mode: onion and `.i2p` results as well as clearnet ones, typed addresses too,
and Hyphanet pages through your node, as `http://hyphanet/USK@…` (a key typed
into the address bar opens there too). Every request goes through the daemon,
over Tor (or i2pd for `.i2p`, the Hyphanet node for `hyphanet`), with
each site on Tor circuits of its own; ⋮ → New identity closes every tab and
gives every site new circuits. GeckoView makes the APK much larger; see
[Known limitations](#known-limitations).

The browser has tabs, up to 16. A result opens in a tab of its own, and Back
at its first page closes it and returns to the search page; other tabs stay
open, and the framed count beside the address (or on the search page's
toolbar) lists them. Long-press a link to open it in a new or background tab,
or to copy it; a page that opens a window gets a tab next to it. Words typed
into the address bar that are not an address search OMNId's index. ⋮ has
Reopen closed tab (the last ten, kept in memory only), find in page, bookmarks
(the one thing the browser keeps, and only when asked), share and copy link (a
Hyphanet page as its `freenet:` key), Desktop site (the page's computer layout,
per tab) and the security level. Video plays in the page or full screen, its
own controls' button hiding the toolbar and the system bars until Back; the
levels below decide whether it waits for a tap.

A file the browser does not show (an archive, a package, a document) is a
download, after it asks: the file's name, size and site, and a warning that
another app opening it can go online outside Tor. It goes to the phone's
Downloads, with a notification showing progress and Cancel; OMNId never opens
it. A file of 2 MB or more from a server that serves byte ranges is fetched by
the daemon in parallel ranges (Settings → Download connections, 4 by default),
each on Tor circuits of its own, which on the emulator took a 21.8 MB file from
an onion mirror in 60 seconds where one connection takes about two and a half
minutes; a smaller file, or one from a server without ranges, comes over the
browser's own connection. If the daemon's answer is not the file the browser
saw (another size or version), the browser's copy is used instead. Either way
nothing goes direct. The levels:

- **Standard**: every site feature, with resist-fingerprinting.
- **Safer**: no JIT, WebAssembly, MathML or SVG fonts; media plays only when
  tapped; no JavaScript on a page that could be changed on the way (plain HTTP
  from the clearnet, which HTTPS-only mode already refuses).
- **Safest**: no JavaScript, SVG or site fonts.

A new level reloads every open tab.

## Layout

| Path | Owns |
|---|---|
| `crates/omnid-core` | `Source` (canonical address + network), `IndexedDoc` |
| `crates/omnid-net` | `Gateway::fetch`, `Gateway::connect` and `Gateway::download`: transports, redirects, the private-address guard, downloads in parallel byte ranges |
| `crates/omnid-index` | Tantivy schema, analyzer, ranking, snippets, mirror folding |
| `crates/omnid-crawl` | crawl store, frontier, robots.txt, extraction, safety gate, crawl loop |
| `crates/omnid-dht` | read-only DHT client: get_peers, BEP 9 metadata, BEP 51 sampling, own bencode |
| `crates/omnid` | the daemon as a library (`omnid::start`) and the CLI; API, search page, SOCKS proxy |
| `crates/omnid-android` | JNI bridge for the app |
| `android/` | the Android app (Kotlin, platform APIs only, plus GeckoView for the browser) |
| `scripts/` | the emulator smoke test, the live router tests, the app icons |
| `docs/` | [benchmarks](docs/benchmarks.md) and [how a release is cut](docs/releasing.md) |

## Design notes

- **Redirects are resolved against the source that issued them.** A clearnet
  page redirecting to an onion is followed over Tor. The crawler takes
  redirects unfollowed and queues the target, so it is deduplicated,
  rate-limited and checked against the link policy like any link.
- **The crawl survives restarts.** Every known address lives in SQLite with
  its next visit, consecutive failures and liveness; the in-memory frontier is
  a bounded working set pulled from it, shallowest first. Pages are revisited
  after 3 days (onion, I2P, Nostr, magnets) or 7 (clearnet, Hyphanet, IPFS), failures back
  off from an hour to 30 days, 404/410 leave the index, and a router that is
  not running only defers its sites by 10 minutes.
- **Ranking** multiplies BM25 by provenance and reliability:
  `(0.5 + trust) × (0.6 + 0.4 × liveness)`. Operator seeds have trust 0.8,
  discovered links 0.5; liveness is a moving average of whether the address
  answered. Text relevance still dominates.
- **Mirror folding.** `content_hash` is BLAKE3 over the page's normalized
  visible text, so copies that differ only in markup or link targets share
  it. Matching is exact; near-duplicates need a similarity hash later.
- **Wide before deep.** Depth is counted twice: `--max-depth` (default 5)
  is how many sites the crawl goes from a seed, a link to another site being
  one hop and links within a site none, and `--max-site-depth` (default 4) is
  how many links deep it goes into each site from the page it entered by.
  `--max-site-pages` (default 500) caps the addresses kept per site, so one
  big site cannot fill the crawl, and sites take turns in the queue, each
  one's shallowest page first. The phone uses 4, 3 and 150.
- **Default seeds** for a new store given none: services' own onion sites
  (the Tor Project, with its list of its onion services, support and
  community portals and Tor Metrics; Debian's list of its onion services,
  blog and micronews; the BBC, ProPublica, Bellingcat, Amnesty International,
  the Internet Archive, SecureDrop, Riseup, Systemli, Nos oignons, Proton,
  Mullvad, Privacy Guides, Qubes, Whonix and its forum, Kicksecure, Cwtch,
  OnionShare and its documentation, F-Droid, Bitcoin Core, the Tor
  specifications, Tor research and Snowflake, MetaGer, FragDenStaat, the
  Monero documentation, Wasabi Wallet, AirVPN and Njalla) and Ahmia's list of
  onion services; I2P's project site, wiki, news, forum, registries, the
  notbob.i2p and isitup.i2p directories, the Russian community's wiki, I2P+,
  I2P's Git forge and Postman's mail service, with Let's Decentralize, the
  discuss.i2p forum, Shadow Wiki, the Anthrill wiki, The Cypherpunk Handbook,
  the I2P Learning Lab, the sites of i2pd's maintainers acetone and R4SAS, the
  I2P plugin repository, Free Software Fans, OS3, ArchWorks, EasyGPG, Xeres,
  Kestrel, Revuo Monero and Programthink's blog; and the Hyphanet node's
  default bookmarks, Clean Spider and the Index of Indexes among them, with
  the Freenet Documentation Wiki, Sone, Publish!, Bob's Safe Closed Index, the
  Freenet Information Portal, the Freenet Applications FreeSite, the Frost FAQ,
  the Unofficial Guide to FMS, Bombe's flog and jSite, Arne Babenhauserheide's
  site, Sharesite, FLIP and dgof; and on IPFS its documentation,
  specifications, blog and downloads, and the Distributed Wikipedia Mirror in
  English, Turkish and Russian; and on Nostr fiatjaf, jb55, Vitor Pamplona,
  Edward Snowden and jack (`omnid::DEFAULT_SEEDS`).
  An onion site on Ahmia's blocklist is not made a seed (five older seeds are
  on it wrongly; see Safety). An eepsite is named when the address book a new
  i2pd starts with has the name, and otherwise given by its b32 address, which
  any router resolves. A network's seeds wait while its router is down.
- **Politeness.** Per network: clearnet 32 concurrent with 1 s per site,
  onion 16 with 5 s, I2P 4 with 5 s, Hyphanet 2 with 2 s, IPFS 8 with 1 s,
  Nostr 4 with 1 s (the phone uses 6/8/3/2/2/2). One request per site at a time. robots.txt is honored and fetched
  over the same network as the page.
- **Link following.** Overlay and magnet links are always harvested.
  Clearnet links are followed only from clearnet pages, and by default only
  within the same site (`www.` included; `--follow-offsite` lifts that), so a
  seed list of directory pages does not turn into a crawl of the web. IPFS
  links are followed only from pages fetched from this machine's own address:
  the clearnet (without `--clearnet-via-tor`) and IPFS itself.
- **HTML is tokenized, not parsed into a tree.** A tree builder does work
  proportional to nesting depth on every element: a 600 KB page of nested
  `<div>`s took 11.5 s in release. The tokenizer is linear. omnid runs no
  scripts, so `<noscript>` is read as a page shows with scripts off; its text
  comes after the rest of the page's.
- **Forums** spend a site's page budget on threads. Addresses that are a
  forum's sign-in, sign-up, member, profile, search, reply or print pages
  (Discourse, NodeBB, Lemmy, Flarum, XenForo, phpBB, MyBB, vBulletin, FluxBB,
  SMF) are not fetched; crawling 80 addresses each, 45 of Discourse Meta's had
  been user pages. Session IDs in links (phpBB's `sid`, vBulletin's `s`,
  `PHPSESSID`, `jsessionid`) are dropped from the address, so a thread has one.
  A page whose `<link rel="canonical">` names another address on its site, at
  the same path or above it, is crawled there instead (a front page named as
  every page's canonical is ignored). A page with a captcha or bot check
  in its markup and little text besides is not indexed, and is tried again
  later as a failure is.
- **Magnets** are resolved by a read-only DHT node (BEP 43 `ro`, rate
  limited, private and low-port peers skipped). Only the info dictionary is
  fetched over `ut_metadata` and checked against the info hash; no piece is
  ever requested or uploaded. Liveness is the peers found over 20, capped at 1.
- **Pages in the reader's language rank first.** A page's language is the
  one its `<html lang>` declares, or else what its text looks like. Searches
  prefer the languages in `lang=` or the browser's `Accept-Language` (the CLI:
  `--lang`, else `$LANG`); pages in other known languages score lower, and
  pages nearer a site's root a little higher.
- **Downloads** (`omnid download`, and the app's browser) fetch a file in
  byte ranges over several connections at once (`--connections`, default
  4, at most 16). Over Tor each connection gets circuits of its own, so the
  download runs at what several circuits carry together; the browser's
  downloads never go direct, and their first connection stays on the
  site's circuits. The first request asks for `bytes=0-`; when the server
  answers with a length (206), the first connection keeps reading from the
  start, the rest of the file is split among the others, and a connection
  that finishes takes half of the largest part still left, so a slow
  circuit does not hold the download. A connection that fails or sends
  nothing for 30 seconds is dropped and the rest of its part goes to a new
  one on new circuits (up to 4 times in a row without progress, then the
  download fails). Every later answer must be the same file: the same
  length, and `If-Range` with the first answer's ETag or date, so a file
  that changes meanwhile fails the download instead of being spliced from
  two versions. A file under 2 MiB comes on one connection, and a server
  without ranges in one stream, which cannot resume. The name is the
  server's (`Content-Disposition`) or the URL's, made safe: one path
  component, no control characters, no leading dot, at most 200 bytes.
  `--max-bytes` (default 16 GiB) refuses a larger file before anything is
  written. The file goes into a hidden `.part` file beside the target and
  is renamed when complete. An existing file is kept unless `--force`: an
  `-o` file name is checked before the download, a name the server gives
  only once it is known, at the end.
- **Index format is versioned** by directory name (`index-v3`). A schema
  change builds a new index rather than misreading an old one; the daemon
  copies the previous version's pages into it on first start, then removes
  the old directory.

## Anonymity

What OMNId hides, and from whom:

- **Your browsing.** Every page the app's browser opens goes through the
  daemon: the clearnet and onion sites through Tor, each site on circuits of
  its own; `.i2p` through your I2P router, each site from an I2P identity of
  its own where the router has SAM on; Hyphanet pages through your node.
  Nothing in the browser connects directly: its Gecko prefs send whatever the
  proxy extension does not route to a dead-end port, checked before a page
  loads. It resists fingerprinting as Firefox's resist-fingerprinting does,
  which is not Tor Browser (see Known limitations).
- **Your crawling.** The crawler follows what it finds, so content on one
  network could make it connect on another: a page on an onion site linking to
  a clearnet address its author watches, or to a magnet the author follows on
  the BitTorrent DHT, would show the author your IP if the crawler fetched or
  looked it up directly. On the phone the crawler reaches web pages through Tor
  (Settings → Crawl the web through Tor) and the DHT is off (Settings → Torrent
  lookups), unless you change them. A computer running `omnid serve` keeps its
  defaults (direct clearnet, the DHT on), says so in its log at start, and
  takes `--clearnet-via-tor --no-dht` for the same protection.
- **Your other apps**, through the private proxy (Settings → Proxy for other
  apps, or `--socks-password`): never direct, each username on circuits, and
  with SAM an I2P identity, of its own.
- **One eepsite from the next.** Through a router's SAM bridge, each site the
  browser is on gets a destination of its own, as each gets Tor circuits of
  its own: new, never published, on 3-hop tunnels whatever the router's
  defaults (the i2pd app's are 1 hop), and closed after ten minutes unused or
  at New identity. What a page loads from other eepsites comes from the
  page's site's destination. Two are kept made ahead, since one takes 5 to 60
  seconds to build; a site's first page waits only when both are taken. The
  crawler keeps the router's HTTP proxy and its destination, so it never
  shares one with your browsing.
- **Downloads** go the browser's way, never direct. Their parallel
  connections use circuits of their own, so more exits see parts of a file
  being fetched (encrypted where the site uses HTTPS), each only its part.
- **Where you came from.** A redirect from an onion, I2P or Hyphanet site to
  the clearnet is never followed directly (only through Tor exits); a request
  that crosses from one network to another carries no Referer and
  `Origin: null`; pages see `Accept-Language: en-US,en;q=0.5` whatever the
  phone's languages; and a page cannot, by script or redirect, send the
  browser into I2P or Hyphanet or open another app: that takes a tap or
  typing. A magnet goes to a torrent app only after a dialog, and without the
  trackers and peers a page can put in a link.
- **One visit from the next.** New identity closes every tab, gives every
  site new circuits and ends the connections still open on the old ones;
  TLS sessions are not resumed. A request that cannot say which page it
  belongs to gets circuits of its own rather than sharing one with others.

What it does not hide:

- **That you use Tor, I2P and Hyphanet.** Your network sees connections to Tor
  guards, to I2P peers or your router elsewhere, and to your Hyphanet node.
  There are no Tor bridges yet.
- **What your I2P router and Hyphanet node fetch for you.** Run them on a
  machine you control; the setup in Quick start keeps them on your tailnet for
  your own devices only.
- **Which Nostr events the crawler asks for**, from your address unless the
  clearnet goes through Tor: relays see what they are asked.
- **What your IPFS node fetches.** IPFS is not an anonymity network: its
  peers see your node's address ask for each CID. OMNId's browser never
  reaches IPFS, and the crawler goes there only from pages it fetched from
  your own address anyway.
- **A file you open in another app**, which may go online by itself, and the
  torrent app a magnet is handed to, which goes online directly.
- **One I2P identity, without SAM.** A router without SAM on, such as the
  i2pd app as it ships, leaves its HTTP proxy, which sends everything through
  one I2P destination: the browser's eepsites, the crawler's and the apps
  behind the proxy, so eepsites can link those visits to each other (though
  not to your IP). The search page's I2P row says which you have, and offers
  Settings → I2P router when it is one. The i2pd app's proxy also has 1-hop
  tunnels, where I2P's own default is 3, and one lasting address
  (`keys = proxy-keys.dat`) that its SOCKS proxy shares: easier to trace, and
  linkable from one day to the next. In its `i2pd.conf`, set `enabled = true`
  under `[sam]` (and, for the crawler and other apps, the `[httpproxy]`
  section's `inbound.length` and `outbound.length` to 3 and `keys` to
  `transient`), or use a router on your own computer.
- **How OMNId downloads.** The daemon's download requests differ from the
  browser's own (their headers, HTTP/1.1, several exits within a second), so a
  server can tell an OMNId download apart.
- **An adversary watching both ends** of a Tor, I2P or Hyphanet connection,
  who can match them by timing. These networks do not defend against that,
  and neither does OMNId.

## Safety

- **The crawler never downloads media.** Only `text/html`,
  `application/xhtml+xml` and `text/plain` bodies are read; the check runs on
  the response headers before the body is fetched. A missing Content-Type is
  refused. A file of any type is downloaded only when you ask for it
  (`omnid download`, a download in the app's browser).
- **Denylist** (`--denylist`): hostnames (subdomains included), Hyphanet keys,
  IPFS CIDs and IPNS names (`ipfs://…`, `ipns://…`), Nostr authors (`nostr:npub1…`, which
  keeps out everything they signed), magnets, or MD5 hex digests of hostnames, the form published blocklists use.
  Denied sites are never fetched and links to them are never queued.
- **Ahmia's abuse blocklist**, the onion services reported to Ahmia for
  abuse (about 60,000, as MD5 digests), is kept in the denylist: the last copy
  from the data directory at start, then fetched over Tor from Ahmia's onion
  site and refreshed daily; a list of fewer than 1,000 entries is taken for a
  broken answer and the held one kept. `--no-blocklist` leaves it out. It is
  someone else's judgement and is not always right: in November 2026 it named
  the onion sites of ProPublica, Proton, Qubes and Riseup. So it never
  overrules your seeds (the default ones and `--seeds`): they and their
  subdomains are crawled whatever it says, and what it blocked on them before
  is crawled again at the next start. Your own `--denylist` still applies to
  them. Turned off (`--no-blocklist`, or Settings → Ahmia's blocklist in the
  app), the held copy is deleted and what it blocked is crawled again, once;
  your own lists still decide, and turned back on it is fetched anew. A bare top-level name in it ("onion") is ignored, so one bad line
  cannot shut a whole network.
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
- The proxy is off unless `--socks` is given. Without `--socks-password` it has
  no authentication and should stay on loopback; with one it refuses clients
  without it and never connects directly.

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
  Firefox's resist-fingerprinting is on, pages get en-US whatever the phone's
  language, WebRTC, WebGL, WebGPU, geolocation, motion sensors, gamepads, DRM,
  beacons and prefetching are off, lookalike names show as punycode, HTTPS only on the clearnet (onions are exempt, and a
  `.i2p` site without HTTPS is reloaded over HTTP: I2P, like Tor for onions,
  encrypts the whole way), strict tracking protection, no Safe Browsing lookups. Site permissions (camera,
  microphone, location, notifications) are refused, a file is downloaded only
  after you say yes to it, and screenshots and the recent-apps thumbnail are
  blocked unless allowed in Settings.

## Known limitations

- HTTPS onions need a certificate that validates; expired or self-signed ones
  are refused. Facebook's onion rejects the TLS handshake (HTTPS onions such
  as BBC's work).
- The open `--socks` proxy does not isolate streams per client the way Tor
  Browser does; everything shares circuits. A private one (`--socks-password`)
  isolates by username, and the Android browser's proxy by site.
- The Android browser is not Tor Browser. It runs release GeckoView with
  Firefox's own fingerprinting protection, not Tor Browser's patched ESR, so a
  site can tell it apart from Tor Browser and from other omnid users only
  as far as resist-fingerprinting allows. Use Tor Browser where that matters.
  GeckoView must be updated with every Firefox release, which carries its
  security fixes (`android/app/build.gradle.kts`).
- On the phone, `.i2p` needs the i2pd app with its router running, or a router
  elsewhere under ⋮ → I2P router; omnid cannot start the app's router (i2pd
  offers other apps no way to).
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
`bench_crawl`, `bench_search`, `bench_tor`, `bench_download` and the DHT `soak`.

Releases are cut as [docs/releasing.md](docs/releasing.md) describes, and the plan is in
[ROADMAP.md](ROADMAP.md). Agents changing the code start from [AGENTS.md](AGENTS.md).

## License

MIT, see [LICENSE](LICENSE). Dependencies are permissively licensed (MIT,
Apache-2.0, BSD, ISC, Zlib, Unicode, CC0, the Unlicense, and CDLA-Permissive-2.0
for `webpki-roots`' root certificates) except `option-ext`, an unmodified
MPL-2.0 dependency of arti, and GeckoView in the Android app, MPL-2.0 and
unmodified (its licenses are at `about:license` in the browser). The app's Java
and Kotlin libraries, which come with GeckoView (AndroidX, Media3, Guava,
kotlinx.coroutines, the Kotlin standard library), are under Apache-2.0. Each
release ships the notices of all of these: `THIRD-PARTY-NOTICES.txt` in the macOS
tarball, and Settings → About → Licenses → Third-party notices in the app, both
written by `scripts/third-party-notices.py`. The typeface is Space Mono, under the SIL Open Font License 1.1
(`crates/omnid/src/ui/space-mono-OFL.txt`, and in the APK's
`assets/licenses/`); the font files carry it too.

# Changelog

## Unreleased

- The app's browser has tabs (up to 16) and a tab list, in a task of its own: a result opens in a
  new tab, Back at its first page returns to the search page, and the search page's toolbar shows
  the open tabs. Long-press a link to open it in a new or background tab or copy it.
- Find in page, bookmarks, copy link, and Tor Browser's three security levels (Standard, Safer,
  Safest), which turn off JavaScript, JIT, WebAssembly, SVG and site fonts step by step.
- Less to fingerprint: pages get en-US whatever the phone's language, and motion sensors,
  gamepads, WebGPU, DRM and beacons are off; lookalike names show as punycode.
- New identity closes every tab, after asking.
- Tapping the address bar selects the whole address, so what is typed replaces it.

## 0.1.1 (2026-11-21)

- The app and the search page write the name OMNId; the command and package stay `omnid`.
- The app's notification says what crawling is waiting for (a charger, Wi-Fi) instead of a fixed
  line.

## 0.1.0 (2026-11-21)

The first release: the daemon and CLI, and the Android app. Run on an Android 16 emulator and on
macOS; not yet on a physical phone.

### Daemon and CLI

- One gateway for the clearnet (with a private-address guard), Tor (arti, in-process, a stream
  isolation token per site), I2P through i2pd, and Hyphanet through a node's FProxy, following
  redirects across networks.
- A read-only BitTorrent DHT client that resolves magnets to names and file lists; media is never
  downloaded.
- A crawler with a SQLite store, politeness per network, robots.txt, a safety gate (denylist and
  blocked terms), and pages put off rather than failed while a router is down, then crawled as soon
  as it is back.
- One Tantivy index for every network: mirrors folded into one result with several routes, ranking
  by trust, liveness, depth and the reader's languages; words, phrases, `-exclude`, `title:`,
  `site:`, `network:`; the word being typed matches as a prefix; close spellings when nothing
  matches.
- `omnid serve`: the HTTP API (`/api/search`, `/api/submit`, `/api/crawl`, `/api/status`,
  `/hyphanet/<key>`), the search page, and an optional SOCKS5 proxy that also reaches the Hyphanet
  node as `http://hyphanet/<key>`.

### Android app

- The daemon in a foreground service, crawling on a charger and Wi-Fi unless allowed always; seeded
  with the Tor Project's and the BBC's onion services and I2P's hubs.
- The search page in the icon's colours: a network strip that explains each router, chips ordered
  by results, Explore by site, readable addresses, and a search field that opens or adds an address.
- A private browser (GeckoView): every request through the daemon, over Tor, i2pd or the Hyphanet
  node and never directly; HTTPS-only on the web; resist-fingerprinting; New identity.
- I2P through the i2pd app, which it detects and points to; Hyphanet through a node on another
  machine (⋮ → Hyphanet node).

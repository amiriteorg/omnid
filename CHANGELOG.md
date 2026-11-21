# Changelog

## Unreleased

- Fixed: Ahmia's blocklist kept the crawler off five of the default onion seeds. The list, published
  as onion services blocked for child sexual abuse material, names ProPublica, Proton, Qubes,
  Riseup and one more of them; on a phone running 0.1.2 their pages were refused and taken out of
  the index. The list no longer overrules the seeds (the default ones and `--seeds`) or their
  subdomains, and what it blocked on them is crawled again at the next start; the operator's own
  denylist still applies. A bare top-level name in a published list is ignored, so one bad line
  cannot shut a whole network.
- Several I2P routers and Hyphanet nodes at once: `--i2p-proxy` and `--fproxy` repeat (or take a
  comma-separated list), best first. A request goes to the first router that answers; one that
  cannot be reached rests, longer each time, and is tried last until it is back; a gateway error
  (i2pd's own 5xx pages) hands the request to the next router. The browser's and the SOCKS
  proxy's streams also start on the next router when the first has not answered within 3 seconds.
  The status shows every router.
- Settings (⋮ → Settings, on the search page and in the browser): theme (as the phone, light or
  dark) and text size on web pages; the security level, loading images (off, pages load far faster
  over Tor) and screenshots; the networks a search starts with, the languages ranked first, and
  blocked words and blocked sites for the crawler; crawl speed (Gentle, Normal, Fast) and crawling
  on battery and mobile data; the I2P router and Hyphanet node; the version and licenses.
  `Native.start` takes the crawl speed.
- The browser: words typed into the address bar that are not an address search the index; ⋮ gains
  Reopen closed tab (the last ten, in memory only), Share, Desktop site (per tab) and Settings;
  video goes full screen from its own controls, hiding the toolbar and system bars until Back.
- Fixed: the browser crashed when the tab list, hidden after tabs had opened or closed, was laid
  out again, which switching between a hardware keyboard and touch does.
- The app takes an I2P router elsewhere under ⋮ → Settings → I2P router, such as one on your computer over
  Tailscale, asked before the i2pd app on the phone, which stays the fallback; a Hyphanet node
  set under ⋮ → Hyphanet node now keeps a node on the phone as its fallback too.

## 0.1.2 (2026-11-21)

- Fixed: on the phone, every clearnet page over HTTPS failed. reqwest checked certificates with
  the platform verifier, which on Android panics unless the app hands it its context; the 0.1.1
  crawler rarely left onion sites, so it hardly showed. Every connection now checks certificates
  against Mozilla's roots (webpki-roots) with rustls, as the Tor path already did, which also
  keeps the platform from making revocation lookups of its own outside the gateway.
- The crawler goes wide before deep. Links within a site no longer use up the hops from a seed:
  `--max-depth` (now 5) counts sites, `--max-site-depth` (4) pages within a site, and
  `--max-site-pages` (500) caps any one site; sites take turns in the queue. The phone crawls 4
  sites out, 3 pages into each, at most 150 pages a site. `crawl.sqlite`'s `pages` table gains
  `site` and `site_depth`, added to older stores on open.
- Default seeds on Tor, I2P and Hyphanet, used for a new store; the app adds any it is missing at
  every start.
- Ahmia's list of onion services reported for abuse is kept in the safety gate, refreshed daily
  over Tor (`--no-blocklist` to leave it out).
- Directory pages give up to 20,000 links instead of 500.
- The app's browser has tabs (up to 16) and a tab list, in a task of its own: a result opens in a
  new tab, Back at its first page returns to the search page, and the search page's toolbar shows
  the open tabs. Long-press a link to open it in a new or background tab or copy it.
- Find in page, bookmarks, copy link, and Tor Browser's three security levels (Standard, Safer,
  Safest), which turn off JavaScript, JIT, WebAssembly, SVG and site fonts step by step.
- Less to fingerprint: pages get en-US whatever the phone's language, and motion sensors,
  gamepads, WebGPU, DRM and beacons are off; lookalike names show as punycode.
- New identity closes every tab, after asking.
- Tapping the address bar selects the whole address, so what is typed replaces it.
- A new look, "Xerox": the wordmark and every label, title and address in Space Mono (SIL Open
  Font License, served by the daemon itself), no rounded corners, rules instead of boxes, an ink
  block for what is selected, a highlighter behind unread result titles, and network badges
  outlined in their colour, which they had lost to grey. Same navy palette, light and dark, with
  the light scheme's green and teal a shade darker so small capitals in them read (4.5:1 or more).

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

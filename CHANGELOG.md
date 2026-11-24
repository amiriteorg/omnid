# Changelog

## Unreleased

- Fixed: IPv6 addresses outside the public internet passed the private-address guard. On a
  machine running Yggdrasil, an address in its 200::/7 was fetched directly, as were the discard
  prefix (100::/64), IPv6's newer documentation range (3fff::/20) and 6to4 addresses carrying a
  private IPv4 address. Only global unicast (2000::/3) counts as public now.
- Gemini and Gopher are networks: capsules (`gemini://`) and holes (`gopher://`) on the clearnet,
  onions and I2P, each reached on the network its host is on. Gemini pages, Gopher menus and text
  items are indexed and their links followed; a Gopher item's type is read before it is asked
  for, so binaries never are. Each obeys its own robots.txt, as Gemini's `indexer` and Gopher's
  `veronica` too. New default seeds: Project Gemini's capsule, Kennedy's and Lupa's lists of
  known capsules, an onion capsule, Floodgap's server and list of holes, OGUP's list and SDF. The
  search page and the app have Gemini and Gopher filters, and `/api/status` lists `gemini` and
  `gopher` rows, between `nostr` and `magnet`.
- The reader, `/read?url=…`: a Gemini page or a Gopher menu in the search page and the app, its
  lines shown as text (headings, lists, quotes, preformatted blocks, links), its links followed
  within Gemini and Gopher, and a Gemini page's question answered with a form. It reaches servers
  as the app's browser does, through Tor and never directly.
- A text file's first line, when short, is its title (a phlog post's, an RFC's), instead of its
  address.
- IRC is a network: a directory of channels from each network's channel list, with their topics
  and how many people they have, never what is said in them. omnid registers as a labelled bot,
  lists the channels with 10 or more people and quits; one list, kept 12 hours, serves a
  network's channels. `irc://`, `ircs://` and web chats' links (Libera.Chat's, OFTC's, Kiwi
  IRC's) are addresses; links in topics are followed. New default seeds: Libera.Chat, OFTC and
  its onion, hackint, EFnet, IRCnet, Undernet and Irc2P. The search page and the app have an
  IRC filter; the app hands a channel to an IRC app after a dialog. `/api/status` lists an
  `irc` row, between `gopher` and `magnet`.
- Yggdrasil is a network: sites at addresses in its 200::/7, and Gemini, Gopher and IRC servers
  there, reached through a Yggdrasil node on the same machine (on a phone, the Yggdrasil app's
  VPN) or through yggstack's SOCKS5 proxy (`--yggdrasil-socks`; `--no-yggdrasil` turns it off).
  Nothing else is sent that way, and the app's browser does not reach it. New default seeds
  from Yggdrasil's list of services. A Yggdrasil filter in the search page and the app, and a
  `yggdrasil` row in `/api/status`, between `irc` and `magnet`.

## 0.1.7 (2026-11-23)

- Fixed: an eepsite opened as `http://` that has no HTTPS could leave the app's browser on a blank
  page for a minute or more. The browser tries HTTPS first, and after 3 s Gecko asks the site over
  HTTP so it can stop waiting, but the browser's extension sent that question nowhere. It now lets
  it through, as that site, while a tab is loading the eepsite: on the test emulator reg.i2p
  opened in 6 to 31 s in every run, where it had taken 35 to 63 s, or not opened within 90 s in 5
  runs of 8.
- Fixed: some eepsites, stats.i2p among them, refused the app's browser with "403 Inproxy access
  denied": it sent Gecko's User-Agent, where I2P's proxies send `MYOB/6.66 (AN/ON)` for every
  client. It sends I2P's to `.i2p` sites now.
- Fixed: a Nostr article whose `d` tag was 256 bytes or longer got a wrong address. NIP-19
  writes the tag's length in one byte, so the address named a different article, or nothing. A
  crafted tag could even make it name another author's article. Such an article is now linked
  by its ID, and an `a` tag naming one is skipped.
- When the app's browser or the proxy for other apps cannot reach a site, the daemon logs why
  (`adb logcat -s omnid`): the network and Tor's or the router's reason, never the site, whose
  name is replaced. It had logged nothing a phone shows, so a failure on a real phone could not
  be told apart from another.
- Fixed: a Hyphanet address with whitespace before its `#fragment`
  (`freenet:USK@…/site/3/ #top`) kept the whitespace in its key, so the address the crawler
  stored named another key, or none.
- Proxy clients' host names are checked on plain HTTP requests (`GET http://host/`) too, not
  only on CONNECT and SOCKS: a name such as `a)b.i2p` gets a 400 instead of reaching a router.
- Links whose file name says the crawler would not index them (archives, programs, disk images,
  media, documents, fonts, checksums and signatures, by a URL's last segment or an IPFS gateway's
  `?filename=`) are not followed: asking for one cost a fetch over Tor or I2P, and made an IPFS
  node fetch the file's first blocks. The phone, crawling from IPFS's release downloads
  (`dist.ipfs.tech`), had visited 2,507 release files in six minutes; that seed is gone.
- The app reaches IPFS and Nostr: ⋮ → Settings → IPFS gateway takes an IPFS node's gateway on
  your computer (none by default: no IPFS node runs on a phone), and Nostr's relays are read
  through Tor while "Crawl the web through Tor" is on, with Settings → Nostr relays to turn them
  off. Both have filters in Settings → Search and rows in the search page's status panel. A Nostr
  result opens in a Nostr app after a dialog, as a magnet does in a torrent app; only `nostr:`
  notes, profiles and articles are handed on, never a key.
- Telegram's public channels are crawled through their web previews: a link to a channel or one of
  its posts (`t.me/<name>`, `t.me/<name>/<post>`, `telegram.me`) is the channel's preview,
  `https://t.me/s/<name>`, with its latest posts, and its `?before=` pages go back through older
  ones. A channel is followed from any page the clearnet may be reached from, other sites
  included; bots, invites and Telegram's own pages are not followed. The search page labels them
  Telegram.
- Nostr is a network: notes, long-form articles, profiles and public chat channels (NIP-28, with
  their latest messages), read from relays over WebSocket and crawled through what they mention.
  `nostr:` entities (NIP-19) and pages of web clients that show one (njump.me, primal.net and the
  like) are addresses. Every event's ID and BIP-340 signature is checked, so a relay cannot forge
  or alter one. Relays are reached as the clearnet is, and `--nostr-relay` and `--no-nostr` set
  them. A denied `nostr:npub1…` keeps out all its author signed. New dependency: `k256`
  (RustCrypto, for the signatures). `/api/status` lists a `nostr` row, between `ipfs` and
  `magnet`.
- Forums crawl better. Their sign-in, sign-up, member, profile, search, reply and print pages are
  no longer fetched (on Discourse Meta they had been 45 of the first 80 addresses). Session IDs
  (`sid`, vBulletin's `s`, `PHPSESSID`, `jsessionid`) are dropped from addresses, which changes the
  address a page with one is indexed under. A page that names a canonical address on its site, at
  the same path or above it (`/post/1?scrollToComments=true`, a post's place in its topic), is
  crawled at that address instead. A captcha or bot-check page (MyBB's hCaptcha, Cloudflare's) is
  no longer indexed. `<noscript>` content is read as markup, since omnid runs no scripts: a
  Flarum forum's discussions are all there, and it had yielded 2 links.
- IPFS is a network: `ipfs://<CID>/…` and `ipns://<name>/…` (IPNS keys and DNSLink names), fetched
  and crawled through an IPFS gateway, by default an IPFS node's on 127.0.0.1:8080 (kubo's);
  `--ipfs-gateway` takes others, best first, as `--fproxy` does. A CID in any base is one address
  (CIDv1 in base32), and links through public gateways (`https://ipfs.io/ipfs/…`,
  `https://<CID>.ipfs.dweb.link/…`) are the content itself. An answer without a gateway's headers
  is refused, so another server on the port is not indexed as IPFS. IPFS is not anonymous, so the
  crawler never follows a link into it from an onion, I2P or Hyphanet page, and the app's browser
  never reaches it. New default seeds: IPFS's documentation and the Distributed Wikipedia Mirror.
  The search page has an IPFS filter and opens IPFS results through `/ipfs/…` and `/ipns/…`, which
  redirect to the gateway. `/api/status` lists an `ipfs` router, between `hyphanet` and `magnet`.

## 0.1.6 (2026-11-22)

- Releases carry their third-party license notices, which the README said they should and none
  did: `THIRD-PARTY-NOTICES.txt` in the macOS tarball, and Settings → About → Licenses →
  Third-party notices in the app, for the Rust crates and the Java and Kotlin libraries inside
  each. `scripts/third-party-notices.py` writes them at release time.

## 0.1.5 (2026-11-22)

- Fixed: since 0.1.2, `omnid serve` stopped at once with "unable to open database file" when its data
  directory did not exist yet, as on a first run. It makes the directory now, as `omnid crawl` did.

## 0.1.4 (2026-11-22)

- Each I2P site the app's browser opens gets an I2P identity of its own, as each gets Tor circuits
  of its own: through the router's SAM bridge, a new destination per site, never published, on
  3-hop tunnels whatever the router's defaults, closed after ten minutes unused or at New identity.
  The proxy for other apps gets one per username. Two are kept made ahead, since one takes 5 to 60
  s to build, and a site's first stream is tried again when a new destination does not find its
  leaseset at first. The crawler stays on the HTTP proxy's destination. Without SAM, everything
  goes through the HTTP proxy as before, and the status says "one I2P identity for every site".
  `--i2p-sam` names bridges (by default port 7656 on each `--i2p-proxy`'s host), `--no-i2p-sam`
  turns them off; the app uses each router's.
- The search page's I2P row says whether sites get identities of their own, and when they share
  one (the i2pd app as it ships: SAM off, 1-hop tunnels) offers Settings → I2P router, whose text
  says how to turn SAM on.
- Anonymity: on the phone, the crawler now reaches web pages through Tor and the BitTorrent DHT is
  off by default (Settings → Anonymity → Crawl the web through Tor, Torrent lookups). The crawler
  follows what it finds, so directly, a page on an onion site could make the phone fetch a clearnet
  address, or look up a magnet on the DHT, that its author watches, and learn the phone's IP. With
  lookups off, magnets are listed from their links alone. A computer running `omnid serve` keeps
  its defaults and warns at start; `--clearnet-via-tor --no-dht` protects it the same way. The
  status says how the crawler reaches the clearnet. The README has a section on what OMNId hides
  and what it does not.
- Security, from an independent audit:
  - A redirect from an onion, I2P or Hyphanet site to the clearnet is never followed directly
    (robots.txt, `omnid fetch`, `omnid download`, the blocklist fetch all did, from the machine's
    IP), only through Tor exits.
  - A page can no longer open a torrent app, or send the browser into I2P or Hyphanet, by script or
    redirect: that takes a tap or typing, and a magnet goes to a torrent app only after a dialog
    and without the trackers and peers a page can plant in the link.
  - The browser's proxy keeps its port for the life of the app, so no other app can take it
    while the daemon restarts; the search page loads the daemon's own port only.
  - New identity ends the connections still open on the old circuits; requests that cannot say
    which page they belong to get circuits of their own; hosts with a trailing dot no longer slip
    past the rule that keeps pages from loading I2P or Hyphanet resources.
  - Pages see `Accept-Language: en-US,en;q=0.5` whatever the phone's languages, and no Referer
    (and `Origin: null`) when a request crosses between networks.
  - Gecko prefs follow Tor Browser 16 more closely (service workers, push, TLS session identifiers
    and remote settings off, among others; Safer also turns off the native regexp engine).
  - A search with a huge offset could abort the daemon; the offset is capped at 10,000.
  - Proxy clients' host names are checked before they reach a router.
  - The search page and Settings follow Allow screenshots; download notifications keep the file
    name off the lock screen; the search page's fields keep the keyboard from learning.
- A router or node at a Tailscale address (100.64.0.0/10) is asked only while this device is on a
  tailnet (an address in that range on a tunnel interface): carrier-grade NAT uses the same range,
  and with Tailscale off on mobile data, plain HTTP to the router could reach another subscriber's
  device. Requests go to the next router instead, and the status says why.
- The browser's downloads send its own Accept, Accept-Language and Sec-GPC.
- The app and README say what the i2pd app's defaults (1-hop tunnels, one lasting I2P address)
  mean, and what to change.
- 38 more default seeds, each answered through its network on 2026-11-21: 10 onion sites (the Tor
  specifications, Tor research, Snowflake, OnionShare's documentation, MetaGer, FragDenStaat, the
  Monero documentation, Wasabi Wallet, AirVPN, Njalla), 17 eepsites given by b32 address so that
  any router resolves them (Let's Decentralize, discuss.i2p, Shadow Wiki and more) and 11
  freesites from Clean Spider (Bob's Safe Closed Index, guides to Frost and FMS, Hyphanet
  developers' sites and tools). None is on Ahmia's blocklist.

## 0.1.3 (2026-11-21)

- `omnid download <address> [-o PATH] [--connections N] [--max-bytes N] [--force]`: a file in
  byte ranges over several connections at once (4 by default, up to 16), each on Tor circuits of
  its own, so a download over Tor runs at what several circuits carry together. A connection that
  finishes its part takes half of the largest part left; one that fails or stalls for 30 seconds
  hands the rest of its part to a new connection on new circuits. `If-Range` and the file's length
  are checked on every answer, so a file that changes meanwhile fails the download instead of
  being spliced. A server without ranges is read in one stream. The engine is
  `omnid_net::Gateway::download`. The crawler still never downloads media.
- Downloads in the app's browser: a file the browser does not show is saved to the phone's
  Downloads after it asks (name, size, site, and a warning that other apps opening it can go online
  outside Tor), with a progress notification and Cancel. From 2 MB, a server that serves ranges
  gets the daemon's parallel ranges (Settings → Download connections, 4 by default), each on
  Tor circuits of its own; anything else, or an answer that is not the file the browser saw, comes
  over the browser's own connection. `Native.download`, `downloadState`, `downloadError`,
  `cancelDownload` and `forgetDownload` are new.
- Ahmia's blocklist can be turned off in the app (Settings → Ahmia's blocklist; on by default), as
  `--no-blocklist` does on a computer. Turned off, the held copy is deleted and what the list
  blocked is crawled again, once, unless the operator's own lists refuse it.
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
- A secure proxy for other apps. The proxy speaks HTTP as well as SOCKS5 on the same port (CONNECT,
  and absolute-form requests passed on in origin form). `--socks-password` makes it private: only
  clients with the password get in, nothing goes direct, and each username gets Tor circuits of
  its own. In the app, Settings → Proxy for other apps serves this at 127.0.0.1:7172 with a
  password made on the phone. Every proxy listener now drops a client that says nothing for 15 s
  and serves at most 512 at once. `Native.start` takes its settings as one JSON object.
- Settings (⋮ → Settings, on the search page and in the browser): theme (as the phone, light or
  dark) and text size on web pages; the security level, loading images (off, pages load far faster
  over Tor) and screenshots; the networks a search starts with, the languages ranked first, and
  blocked words and blocked sites for the crawler; crawl speed (Gentle, Normal, Fast) and crawling
  on battery and mobile data; the I2P router and Hyphanet node; the version and licenses.
- The browser: words typed into the address bar that are not an address search the index; ⋮ gains
  Reopen closed tab (the last ten, in memory only), Share, Desktop site (per tab) and Settings;
  video goes full screen from its own controls, hiding the toolbar and system bars until Back.
- Fixed: the browser crashed when the tab list, hidden after tabs had opened or closed, was laid
  out again, which switching between a hardware keyboard and touch does.
- The app takes an I2P router elsewhere under ⋮ → Settings → I2P router, such as one on your computer over
  Tailscale, asked before the i2pd app on the phone, which stays the fallback; a Hyphanet node
  set under ⋮ → Hyphanet node now keeps a node on the phone as its fallback too.
- More default seeds, each of which answered through its network on 2026-11-21: 17 onion sites
  (the Tor Project's list of its onion services, support and community portals and Tor Metrics;
  Debian's list of its onion services, blog and micronews; Privacy Guides, the Whonix forum,
  Systemli, Nos oignons, Amnesty International, Bellingcat, Cwtch, OnionShare, F-Droid and Bitcoin
  Core), 5 eepsites (the I2P wiki, the I2P-ilita wiki, I2P+, I2P's Git forge and Postman HQ) and 6
  freesites (from the Hyphanet node's bookmarks: the Freenet Privacy and Software Site, FMS and
  Freemail; and the Freenet Documentation Wiki, Sone and Publish!). None of the new onion sites is on Ahmia's blocklist, which a test now checks, and
  each eepsite is in the address book a new i2pd starts with. An older install gains them at
  its next start.

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

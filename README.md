<img src="assets/icon.svg" width="64" height="64" alt="">

# omnid

One daemon that reaches the clearnet, Tor, I2P, Hyphanet and BitTorrent
magnets, crawls them politely, and keeps a single search index over all of
them. Content mirrored on several networks shows up as one result with every
route to it.

```
            ┌──────────── local API + search page (127.0.0.1:7171) ────────────┐
            │                                                                  │
   seeds ─▶ crawler: frontier ─▶ safety ─▶ robots.txt ─▶ fetch ─▶ extract ─▶ safety ─▶ index
                │                                         │                          (Tantivy)
                └────────────── discovered links ◀────────┤
                                                          ▼
                     Gateway::fetch(&Source) routes by network
      clearnet: reqwest │ onion: arti, in-process │ i2p: i2pd HTTP proxy │ hyphanet: FProxy
```

## Status

Phase 1 skeleton. Built and tested:

| Piece | State |
|---|---|
| Address parsing: URLs, bare hosts, v3 onions, `.i2p`, `freenet:` keys, magnets | done, unit tested |
| Clearnet fetch | done, live tested |
| Tor fetch, HTTP and HTTPS, via embedded arti | done, live tested against real onion services |
| I2P fetch via i2pd's HTTP proxy | written; only the "router not running" path is tested |
| Hyphanet fetch via FProxy | written; not tested against a live node |
| Magnets | harvested from pages and indexed by info hash; no metadata yet |
| Tantivy index, network filter, mirror folding | done, unit tested |
| Crawler: politeness, robots.txt, redirects, meta robots | done, end-to-end tested |
| Safety gate | done for text; see [Safety](#safety) |
| Local API and search page | done, live tested |

Not built yet: the DHT module (BEP 5/51 harvesting, BEP 9 metadata), router
lifecycle management (starting i2pd and Hyphanet as child processes),
re-crawl scheduling, a persistent frontier, the local SOCKS/HTTP proxy, index
sync between nodes, the Tauri shell and the Android build.

## Quick start

```sh
cargo build --release
B=target/release/omnid

$B fetch https://example.com/              # one fetch, routed by network
$B fetch http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/
$B crawl --max-pages 50 https://example.com/   # crawl until done, then exit
$B search "example"
$B serve --seeds seeds.txt                 # daemon + http://127.0.0.1:7171
```

Tor starts with the daemon and needs nothing installed. For I2P run i2pd
(HTTP proxy on 4444); for Hyphanet run a node (FProxy on 8888). `--no-tor`,
`--i2p-proxy` and `--fproxy` change that. Data lives in the platform data
directory (`--data-dir` to override): the index in `index-v1/`, Tor state in
`tor/`.

API:

| | |
|---|---|
| `GET /api/search?q=…&network=onion,i2p&limit=20` | results, mirrors folded together |
| `POST /api/submit` with `{"url": "…"}` | queue an address for crawling |
| `GET /api/status` | router states, index size, crawl counters |

## Layout

| Crate | Owns |
|---|---|
| `omnid-core` | `Source` (canonical address + network), `IndexedDoc` |
| `omnid-net` | `Gateway::fetch`: transports, redirects, the private-address guard |
| `omnid-index` | Tantivy schema, upsert, search with mirror folding |
| `omnid-crawl` | frontier, robots.txt, extraction, safety gate, crawl loop |
| `omnid` | the binary: CLI, daemon, API, search page |

A Tauri or Android shell links `omnid-core`, `omnid-index` and, for a full
node, the rest. The daemon's API is the stable seam for everything else.

## Design notes

- **Redirects are resolved against the source that issued them.** A clearnet
  page redirecting to an onion is followed over Tor. The crawler takes
  redirects unfollowed and queues the target, so it is deduplicated,
  rate-limited and safety-checked like any link.
- **Mirror folding.** `content_hash` is BLAKE3 over the page's normalized
  visible text, so copies that differ only in markup or link targets share
  it. Matching is exact; near-duplicates need a similarity hash later.
- **Politeness.** Per network: clearnet 8 concurrent with 1 s per site,
  onion 4 with 5 s, I2P 2 with 5 s, Hyphanet 1 with 2 s. One request per site
  at a time. robots.txt is honored and fetched over the same network as the
  page.
- **Link following.** Overlay and magnet links are always harvested.
  Clearnet links are followed only from clearnet pages, and by default only
  within the same site (`--follow-offsite` lifts that), so a seed list of
  directory pages does not turn into a crawl of the web.
- **HTML is tokenized, not parsed into a tree.** A tree builder does work
  proportional to nesting depth on every element: a 600 KB page of nested
  `<div>`s took 11.5 s in release. The tokenizer is linear.
- **Index format is versioned** by directory name (`index-v1`). A schema
  change opens a new index rather than misreading an old one.

## Safety

- **Media is never downloaded.** Only `text/html`, `application/xhtml+xml`
  and `text/plain` bodies are read; the check runs on the response headers
  before the body is fetched. A missing Content-Type is refused.
- **Denylist** (`--denylist`): hostnames (subdomains included), Hyphanet keys,
  magnets, or MD5 hex digests of hostnames, the form published blocklists use.
  Denied sites are never fetched and links to them are never queued.
- **Blocked terms** (`--blocked-terms`): a matching page is not indexed and
  its links are not followed.
- **Known-abuse media hashes are not checked yet.** PhotoDNA, NCMEC and IWF
  lists are licensed to vetted operators and cannot ship with the code. That
  check has to exist in `omnid-crawl/src/safety.rs` before any media handling
  is added. Operators of a public index should also take legal advice on
  their reporting obligations.

The daemon itself:

- Clearnet fetches refuse loopback, private, link-local, CGNAT and other
  non-public addresses, checked after DNS resolution so rebinding cannot get
  around it. `--allow-private-addresses` turns this off.
- The API binds to loopback and rejects requests whose `Host` is not
  localhost (DNS rebinding). Submissions must be JSON, which a cross-site
  page cannot send without a CORS preflight that is never granted.
- The search page renders crawled text as text only, sends a strict CSP, and
  links only clearnet and magnet routes. Onion and `.i2p` addresses are shown
  for copying, because opening them in an ordinary browser leaks a DNS lookup.

## Known limitations

- HTTPS onions need a certificate that validates; expired or self-signed ones
  are refused. Facebook's onion rejects the TLS handshake (HTTPS onions such
  as BBC's work).
- Bodies are decoded as UTF-8 (lossy); other charsets index poorly.
- The frontier and the seen-set are in memory, and nothing is re-crawled
  within one run.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

No test touches the network: the end-to-end crawl runs against a local server.

## License

MIT, see [LICENSE](LICENSE). Dependencies are permissively licensed (MIT,
Apache-2.0, BSD, ISC, Zlib, Unicode) except `option-ext`, an unmodified
MPL-2.0 dependency of arti; binary distributions should ship the
dependencies' license notices.

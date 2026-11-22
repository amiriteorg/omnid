# Roadmap

Where OMNId goes next, in the order the work is planned. What has shipped is in
[CHANGELOG.md](CHANGELOG.md); what works today, and how it was tested, is in the README's
[Status](README.md#status). Items marked **needs a go-ahead** wait on the owner's decision
before any work starts.

## Where it stands: 0.1.5

One daemon and an Android app search the clearnet, Tor, I2P, Hyphanet and BitTorrent magnets
from one index, with a private browser in the app.

- Each browser site gets Tor circuits of its own, and an I2P identity of its own where the
  router offers SAM.
- On the phone the crawler goes through Tor, and torrent lookups are off by default.
- Every release is a pre-release. Each has run on an Android 16 emulator and on macOS, not
  yet on a physical phone.

## Next

1. **Run it on a physical phone.** Every release so far was tested on an emulator. A real
   arm64 phone should cover:
   - per-site I2P identities through the i2pd app's own SAM bridge;
   - the Tailscale address rule, with Tailscale on and off;
   - crawling on a charger: heat, battery, and whether Android stops the service.

   This needs the owner's phone.
2. **Typed `http://` eepsites without the wait.** The browser's HTTPS-only mode tries
   `https://` first, which took about 25 s on the test eepsites before the page loaded over
   HTTP.
   - `.i2p` and the Hyphanet host should load as typed, as onions already do.
   - The clearnet stays HTTPS-only.
3. **Builds anyone can check.** Releases are built on one Mac and signed by hand
   ([docs/releasing.md](docs/releasing.md)).
   - Build the binaries in CI from the tag, and check them against the local build: the APK
     build is reproducible.
   - Publish build provenance.
   - Add Linux binaries (x86_64 and aarch64).
   - The signing key stays offline.
4. **Tor bridges** (**needs a go-ahead**). They are for networks that block Tor; arti 0.48,
   which OMNId already uses, supports them. There are three phases, each depending on the one
   before.
   - **A: the daemon and desktop.**
     - Turn on arti's `bridge-client` and `pt-client` features.
     - Add the flags `--bridge "obfs4 …"` (repeatable), `--transport <names>=<path to
       lyrebird>` and `--bridges builtin`.
     - The status says "Tor via bridge (obfs4)".
     - Live-test against Tor's built-in obfs4 and Snowflake bridges, using lyrebird built from
       a pinned tag.
   - **B: Android.**
     - Ship lyrebird, built with the NDK from a pinned, verified tag, as a library arti starts.
       It covers obfs4, meek, Snowflake and WebTunnel, for about 10–15 MB. IPtProxy is the
       fallback.
     - Settings → Tor → Connect offers Direct, Snowflake, built-in obfs4, bridges from Tor, or
       your own bridge lines.
     - The built-in list ships in the APK and is refreshed over Tor.
   - **C: Connection Assist.** If Tor cannot connect within a couple of minutes, OMNId offers
     to try bridges.
     - It asks Tor's circumvention API for your country's settings, through the built-in meek
       bridge, and applies them, as Tor Browser does.
5. **A built-in I2P router** (**needs a go-ahead**). The aim is to embed
   [emissary](https://github.com/eepnet/emissary), a Rust I2P router (MIT), in the daemon, as
   arti is for Tor. Then the phone needs no i2pd app, and OMNId controls tunnel lengths and
   identities.
   - A spike comes first. It builds emissary for Android arm64 and measures:
     - memory and CPU;
     - tunnel-build success;
     - time to the first eepsite;
     - battery use.
   - The router would be client-only, relaying nothing for others, and would reseed through
     Tor.
   - I2P's own overview still rates emissary experimental. If the numbers aren't good
     enough, the SAM path stands as it is.

## Later

- **Routers on a computer:** start and stop i2pd and a Hyphanet node as child processes, so
  `omnid serve` needs no separate setup.
- **Near-duplicate detection:** a similarity hash, so near-copies of a page fold into one
  result as exact mirrors already do.
- **Index sync** between your own nodes, such as a phone and a computer.
- **A desktop app shell** around the daemon and the search page.
- **Per-client isolation on the open `--socks` proxy**, which shares circuits between every
  client today. The private proxy (`--socks-password`) already isolates by username.
- **More Android ABIs:** x86_64, for Chromebooks and x86 emulators. The app is arm64 only.
- **A signed and notarized macOS binary.**

## Dates and recurring work

- **Every Firefox release:** bump GeckoView, which carries the security fixes. Dependabot
  watches it, and `maven-metadata.xml` on maven.mozilla.org is checked before each release.
- **By 2027-02-21:** the reviewed advisory exceptions in `osv-scanner.toml` expire. Re-check
  each one.
  - The SQLite CVE clears once arti's rusqlite brings `libsqlite3-sys` 0.39 or later.

## Not planned

- **Downloading media while crawling, or fetching torrent pieces.** The crawler reads only
  indexable text, and the DHT client only fetches info dicts. A file is downloaded only when
  you ask for it (`AGENTS.md`, Architectural invariants).

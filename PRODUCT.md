# Product

<!-- impeccable:product-schema 1 -->

## Platform

android

The Android app leads. Its search page is the daemon's own web page (`crates/omnid/src/ui`),
shown in the app's WebView and, unchanged, in desktop browsers pointed at `omnid serve`; where
the two disagree, the phone wins. The app's native screens (start-up, the browser's toolbar)
use platform Android APIs only, plus GeckoView for the browser.

## Users

Privacy-minded people on their phones who want to search and visit onion, I2P, Hyphanet and
clearnet sites, and see what torrents hold, safely and without being Tor experts.

## Product Purpose

One app that reaches the clearnet, Tor, I2P, Hyphanet and BitTorrent magnets, crawls them into a
single local search index, and opens results in a private browser that only goes through Tor or
I2P. A good session is one where the person discovered what is out there: they browsed across
networks and followed leads, more than found one exact answer.

## Positioning

Search and browsing of the "deep" networks that runs entirely on the user's own phone: its own
crawler, its own index, Tor built in (arti, in-process), each site on Tor circuits of its own,
nothing sent to a search provider.

## Operating Context

- The daemon runs in a foreground service; crawling follows the charger and unmetered networks
  unless the user allows crawling anytime.
- A new install seeds itself with the Tor Project's and the BBC's onion services; the index
  starts small and grows as the phone crawls. Searches answer in milliseconds from the local
  index.
- Routers have states the user can see: Tor bootstrapping or ready, the DHT, and I2P and
  Hyphanet unreachable unless a router runs (phones normally have none).
- People share onions, magnets and links into the app to crawl them.
- Results carry routes (the same page on several networks), the page's language, and first and
  last seen dates. Magnets carry file lists and sizes; media is never downloaded.

## Capabilities and Constraints

- Search syntax: words (falling back to any word), `"phrases"`, `-exclude`, `title:`, `site:`,
  `network:`; per-network counts; paging; results in the reader's language first.
- The page renders crawled text as text only, under a strict CSP (`default-src 'none'`, scripts,
  styles and images from its own origin). No external fonts, scripts or images can load.
- Onion and `.i2p` addresses are never links in an ordinary desktop browser; in the app they open
  in omnid's browser.
- Native Android code uses platform APIs only (no AndroidX or Material components of its own).
- arm64 only; minSdk 29, targetSdk 37.

## Brand Commitments

- Name: omnid, lower case.
- Icon: `assets/logo.png`, four interlocking glowing rings in periwinkle, lavender and cyan on
  deep navy (`#0B0D1F` is the launcher background). The user asked for the app's colors and feel
  to come from it.
- The look the user asked for, in their words: "a modern sleek trustworthy super fast UI/UX using
  the Icon/logo colors to create a coherent feel and optimal experience searching". Craft bar:
  Arc / Arc Search, Signal, Proton.

## Evidence on Hand

- Real crawled content only; no sample results, testimonials or usage numbers exist and none
  may be invented.

## Product Principles

1. Private by construction: nothing leaves except through Tor or I2P, and the UI never makes an
   unsafe action the easy one.
2. Exploration first: make following leads across networks effortless.
3. Honest state: show what the routers and crawler are doing, in plain words.
4. Calm for non-experts: no jargon a privacy-minded non-specialist would need to look up.

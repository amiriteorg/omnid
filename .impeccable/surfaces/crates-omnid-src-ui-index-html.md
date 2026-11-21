---
version: 1
slug: "crates-omnid-src-ui-index-html"
primary_target: "crates/omnid/src/ui/index.html"
related_targets: ["crates/omnid/src/ui/app.css","crates/omnid/src/ui/app.js","android/app/src/main/kotlin/cc/wckd/omnid/MainActivity.kt","android/app/src/main/kotlin/cc/wckd/omnid/BrowserActivity.kt"]
---

# Search page and app chrome

Scope: the daemon's search page (`crates/omnid/src/ui`), shown in the Android app's WebView and in
desktop browsers, plus the app's native chrome (start screen, toolbar, the browser's toolbar).
Mode: Operate. Phone first.

Audience and task: privacy-minded people, not experts, searching and exploring onion, I2P,
Hyphanet, torrent and clearnet results from their own index, then opening them in omnid's
private browser. Craft bar named by the user: Arc / Arc Search (speed, fluidity) and Signal /
Proton (trust, plain security states).

## Direction contract

THESIS: One fast, calm search across every network where trust is visible: each result says which
network it lives on and whether the way there is sealed. It refuses both a themed costume and the
generic grey search app: the icon's navy and glow are the whole palette.

OWN-WORLD: Navy #0B0D1F ground (light scheme: cool white with navy ink), one glow blue for the
primary action and focus, periwinkle for selection, ice for highlights; network colours only as
small dots and badges (onion violet, I2P teal, Hyphanet blue, clearnet ice, magnets amber). System
sans on a 1.2 scale, tabular figures for data, mono only for addresses. 14 to 16 px radii, hairline
rows, soft shadows only on what floats. Drawn SVG icons, one stroke weight.

STORY: They see at a glance that the routers are up and private, type, get results as they type,
and open one in the private browser in one tap. With no query they explore the best sites per
network and can add an address to crawl.

FIRST VIEWPORT: Phone. The toolbar shares the page's ground. A trust strip: Tor, DHT, I2P and
Hyphanet states plus what the crawler is doing. A 52 px search field with icon and clear. A
scrolling row of network chips with counts. Then results (title opens the private browser,
snippet, routes with network badge and lock, language and dates) or, empty, Explore.

FORM: seed: none (pinned) "I want a modern sleek trustworthy super fast UI/UX using the Icon/logo
colors to create a coherent feel and optimal experience searching using the OMNId browser/daemon".
Signature interaction: results update in place as you type, no spinner, with a skeleton only past
150 ms; each router state explains itself on tap.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

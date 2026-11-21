---
name: OMNId
description: One fast search across every network, set like a photocopied zine, with the routers' state on a ruled line.
colors:
  ground: "#0B0D1F"
  surface-2: "#1A1F45"
  line: "#262C57"
  ink: "#EEF2FF"
  ink-2: "#B2BBDF"
  muted: "#8690BD"
  accent: "#95AEFB"
  accent-ink: "#0B0D1F"
  glow: "#4A87FB"
  hi: "rgb(149 174 251 / 0.42)"
  mark: "rgb(149 174 251 / 0.22)"
  mark-ink: "#CFECFB"
  ok: "#4FD18B"
  warn: "#F5B65B"
  bad: "#FF7A86"
  off: "#59628F"
  onion: "#A98BFF"
  clearnet: "#C9D6F5"
  magnet: "#F5B65B"
  i2p: "#45D3D9"
  hyphanet: "#6FA2FF"
  ground-light: "#F5F7FC"
  surface-2-light: "#EDF1FB"
  line-light: "#DDE3F3"
  ink-light: "#0B0D1F"
  ink-2-light: "#3D4670"
  muted-light: "#5D668F"
  accent-light: "#2F5BE6"
  accent-ink-light: "#FFFFFF"
  glow-light: "#2F5BE6"
  hi-light: "rgb(47 91 230 / 0.22)"
  mark-light: "rgb(47 91 230 / 0.14)"
  mark-ink-light: "#0B0D1F"
  ok-light: "#0F7A49"
  warn-light: "#9C5D00"
  bad-light: "#C8283A"
  off-light: "#9AA2C2"
  onion-light: "#6A49DF"
  clearnet-light: "#4A5480"
  magnet-light: "#9C5D00"
  i2p-light: "#0A7279"
  hyphanet-light: "#2A63D6"
typography:
  headline:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "1.35rem"
    fontWeight: 700
    lineHeight: 1.3
    letterSpacing: "-0.04em"
  title:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "1.125rem"
    fontWeight: 700
    lineHeight: 1.35
    letterSpacing: "-0.02em"
  input:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "1.125rem"
    fontWeight: 400
    letterSpacing: "-0.02em"
  title-small:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.9375rem"
    fontWeight: 700
    lineHeight: 1.45
    letterSpacing: "-0.02em"
  body:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.78125rem"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "0.04em"
  meta:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.78125rem"
    fontWeight: 400
    lineHeight: 1.5
    fontFeature: '"tnum"'
  address:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.78125rem"
    fontWeight: 400
    lineHeight: 1.4
  state:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.6875rem"
    fontWeight: 400
    lineHeight: 1.5
    letterSpacing: "0.06em"
  badge:
    fontFamily: '"Space Mono", ui-monospace, "Roboto Mono", Menlo, monospace'
    fontSize: "0.6875rem"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "0.06em"
rounded:
  none: "0px"
spacing:
  hair: "0.35rem"
  tight: "0.5rem"
  row: "0.7rem"
  snug: "0.75rem"
  stack: "0.9rem"
  gutter: "1rem"
  group: "1.5rem"
  section: "2.5rem"
components:
  button:
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0 1.1rem"
    height: "48px"
  button-hover:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0 1.1rem"
    height: "48px"
  button-primary-hover:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
  button-small:
    rounded: "{rounded.none}"
    padding: "0 0.9rem"
    height: "48px"
  button-text:
    textColor: "{colors.accent}"
    typography: "{typography.label}"
    padding: "0 0.75rem"
    height: "48px"
  icon-button:
    textColor: "{colors.muted}"
    rounded: "{rounded.none}"
    size: "48px"
  icon-button-hover:
    backgroundColor: "{colors.surface-2}"
    textColor: "{colors.ink}"
  search-line:
    textColor: "{colors.ink}"
    typography: "{typography.input}"
    rounded: "{rounded.none}"
    padding: "0 3rem 0 2.1rem"
    height: "52px"
  address-line:
    textColor: "{colors.ink}"
    typography: "{typography.address}"
    rounded: "{rounded.none}"
    padding: "0 0.1rem"
    height: "48px"
  chip:
    textColor: "{colors.ink-2}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0 0.8rem 0 0.7rem"
    height: "40px"
  chip-selected:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
  chip-selected-empty:
    backgroundColor: "{colors.muted}"
    textColor: "{colors.ground}"
  route-badge:
    textColor: "{colors.muted}"
    typography: "{typography.badge}"
    rounded: "{rounded.none}"
    padding: "0.2rem 0.45rem 0.2rem 0.35rem"
  route-badge-onion:
    textColor: "{colors.onion}"
  route-badge-web:
    textColor: "{colors.clearnet}"
  route-badge-torrent:
    textColor: "{colors.magnet}"
  route-badge-i2p:
    textColor: "{colors.i2p}"
  route-badge-hyphanet:
    textColor: "{colors.hyphanet}"
  trust-strip:
    textColor: "{colors.ink-2}"
    typography: "{typography.state}"
    rounded: "{rounded.none}"
    padding: "0.45rem 0"
    height: "48px"
  router-row:
    textColor: "{colors.ink-2}"
    typography: "{typography.body}"
    padding: "0.7rem 0"
  result-title:
    textColor: "{colors.ink}"
    typography: "{typography.title-small}"
  result-title-visited:
    textColor: "{colors.ink-2}"
  result-row:
    padding: "1rem 0 0.95rem"
  site-row:
    textColor: "{colors.ink}"
    typography: "{typography.title-small}"
    padding: "0.7rem 0"
  browser-toolbar:
    backgroundColor: "{colors.ground}"
    padding: "6px 2px"
  browser-address-line:
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    height: "48px"
  kind-badge:
    typography: "{typography.badge}"
    rounded: "{rounded.none}"
    padding: "4px 5px 4px 4px"
  tab-count:
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    size: "22px"
  tab-row:
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "6px 2px 6px 12px"
    height: "60px"
  tab-row-current:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
---

# Design System: OMNId

## Overview

**Creative North Star: "The Network Zine"**

OMNId looks like a zine someone photocopied about the networks they explore. It has one monospace face, ruled lines, square marks, and a highlighter run through the titles worth reading. It is meant to look made, not themed. It rejects two looks: the soft rounded app the first build shipped (pills, tonal cards, rounded fields) and the black-and-neon "dark web" costume. It keeps the icon's navy palette in both schemes, so it is still recognisably the same product.

Structure is drawn with lines. A ruled strip carries the routers' state, the search is typed on a line, results are separated by dashed rules, and the only boxes left are controls: outlined buttons, dashed chips, and badges outlined in their network's hue. Wherever something is selected, it becomes a solid ink block. Space Mono sets everything that labels, counts, addresses or heads, and the platform sans is kept for paragraphs. There are no corners, no shadows and no raised surfaces.

The layout is phone first and moderately dense: one column, 48px rows for anything you touch, and capitals only for state and controls. The search page (`crates/omnid/src/ui`) and the Android chrome (`res/values*/colors.xml`, `MainActivity.kt`, `BrowserActivity.kt`) share one token set, one typeface and one set of rules, so the browser's toolbar and tab list read as more pages of the same zine.

**Key Characteristics:**
- The icon's navy palette, with one value per token in each scheme, shared by app.css and colors.xml.
- Space Mono Bold for the wordmark, heads, titles, chips and buttons. Space Mono Regular for addresses, labels and meta. The platform sans for paragraphs.
- Five text sizes: 11, 12.5, 15, 18 and 21.6px (the same in sp natively).
- No corners anywhere: border-radius 0 on every field, control, block and dot.
- Rules instead of boxes: a solid rule opens a section, dashed rules separate items, and a 2px ink line runs under every text field.
- Selection is a solid ink block with ground-coloured text.
- A periwinkle highlighter behind unread titles. Network hues are kept to squares and outlined badges.
- Flat: no shadows and no tonal cards. Motion is short and decelerating, and results update in place.
- Every touch target is at least 48px (48dp).

## Colors

The icon's palette, carried over from the first build and now used like ink on paper: a navy or cool-paper ground, ink in three strengths, one periwinkle accent, state signals and five network hues. Each token has a dark value (the bare name) and a light value (the `-light` name). The scheme follows the system on both platforms.

### Primary
- **Ring Periwinkle** (`accent` #95AEFB; `accent-light` #2F5BE6): the primary button's fill (ADD, OPEN, Android's Try again) with `accent-ink` text (#0B0D1F dark, #FFFFFF light). It also colours the text buttons (SEE ALL, BACK TO EXPLORE, the files disclosure), address links on hover, the trust strip's rules on hover and while open, the highlighter on hover, and the brand mark's strokes.
- **Highlighter** (`hi` rgb(149 174 251 / 0.42); `hi-light` rgb(47 91 230 / 0.22)): the accent at 42% (dark) or 22% (light). It is drawn as a thick underline behind the lower half of an unread result title or Explore site name.
- **Glow Blue** (`glow` #4A87FB; `glow-light` #2F5BE6): focus, the text caret, and progress. Focus is the 2px outline 2px out from any control, and a focused text field's line turning glow and thickening to 4px. Progress is the Android browser's 2dp load line and the start screen's bar. It is never a fill.
- **Periwinkle Wash** (`mark` rgb(149 174 251 / 0.22); `mark-light` rgb(47 91 230 / 0.14)) with **Ice** ink (`mark-ink` #CFECFB; `mark-ink-light` #0B0D1F): matched query terms in titles and snippets, as a square wash with 2px of padding at each side.

### Secondary
State signals for routers, connections and submissions.
- **Signal Green** (`ok` #4FD18B; `ok-light` #0F7A49): a ready router (a filled square and the word READY), the copy button's check for 1.4s, an accepted address, and the browser's TOR badge over HTTPS.
- **Amber Caution** (`warn` #F5B65B; `warn-light` #9C5D00): a connecting router (a filled square that breathes) and its word CONNECTING, NOT ENCRYPTED beside plain-HTTP routes, and the browser's "TOR · NOT ENCRYPTED" badge with an open lock.
- **Coral Alarm** (`bad` #FF7A86; `bad-light` #C8283A): a refused or failed submission.
- **Unlit Slate** (`off` #59628F; `off-light` #9AA2C2): a router that is off (a filled square) or not running (a hollow square with a 1.5px edge).

### Tertiary
Network hues. Each one says where a page lives.
- **Onion Violet** (`onion` #A98BFF; `onion-light` #6A49DF): onion sites.
- **Clearnet Ice** (`clearnet` #C9D6F5; `clearnet-light` #4A5480): the open web, labelled WEB.
- **Magnet Amber** (`magnet` #F5B65B; `magnet-light` #9C5D00): torrents. It shares its value with `warn`; the magnet icon in the badge tells them apart.
- **I2P Teal** (`i2p` #45D3D9; `i2p-light` #0A7279): `.i2p` sites.
- **Hyphanet Blue** (`hyphanet` #6FA2FF; `hyphanet-light` #2A63D6): Hyphanet sites.

### Neutral
- **Deep Ring Navy** (`ground` #0B0D1F; light **Cool Paper** `ground-light` #F5F7FC): the paper. It paints the window, system bars, action bar, browser toolbar and page, and it is the text colour on an ink block. The launcher icon's background (`launcher_background`) is this navy in both schemes.
- **Moonlit White** (`ink` #EEF2FF; light **Navy Ink** `ink-light` #0B0D1F): titles, heads, input text, the 2px field lines, the ink block, toolbar icons and the wordmark.
- **Mist Periwinkle** (`ink-2` #B2BBDF; `ink-2-light` #3D4670): snippets and paragraphs, addresses, visited titles, chip text when off, button outlines, the solid section rules and dashed result rules, and the Android dashed rule.
- **Dusk Slate** (`muted` #8690BD; `muted-light` #5D668F): placeholders, meta, counts, the summary, the crawler's word and resting icons. It draws the dashed rules between Explore sites and between router rows, and it fills a selected chip whose network has no results.
- **Lifted Navy** (`surface-2` #1A1F45; `surface-2-light` #EDF1FB): the square hover wash behind icon buttons, and skeleton bars. It is the only tone besides the ground.
- **Hairline Indigo** (`line` #262C57; `line-light` #DDE3F3): the scrollbar thumb and nothing else. Rules are drawn in ink-2, muted or ink.

There is no raised surface. The old `surface` value is still declared in app.css and colors.xml, but nothing paints it. Don't bring it back as a card or field fill.

### Named Rules
**The Ink Block Rule.** Selection and "on" look the same everywhere: a solid block of `ink` with `ground` text. This covers a selected chip, the shown tab in the tab list, selected text, and a hovered button. Anything inside the block, network badges included, turns to the ground colour. No hue, tint or accent fill marks selection.

**The Squares-and-Outlines Rule.** A network's hue appears only as an 8px square, as a 1px outline around its badge (with the badge's label in the same hue), and in the browser's kind badge. It is never a fill, a row tint or a rule.

**The Shared Ground Rule.** The window, status bar, navigation bar, action bar, browser toolbar and page all paint `ground`, so the app reads as one sheet. app.css and colors.xml carry the same value for the same name in each scheme. A token added on one side takes the other side's value.

## Typography

**Display Font:** Space Mono Bold (with ui-monospace, "Roboto Mono", Menlo, monospace)
**Body Font:** system-ui (with -apple-system, "Segoe UI", Roboto, sans-serif); on Android, the platform sans
**Label/Mono Font:** Space Mono Regular

**Character:** Space Mono is a monospace with typewriter rhythm and odd, characterful capitals. In bold it does the zine's heads and controls, and in regular it sets addresses and meta as if typed. The phone's own sans carries paragraphs so longer reading stays easy. The page serves both weights itself (`/space-mono-regular.woff`, `/space-mono-bold.woff`, `font-display: block`) under `font-src 'self'`, so nothing loads from another origin. Android bundles `res/font/space_mono.ttf` and `space_mono_bold.ttf`. The licence is SIL Open Font License 1.1 (`crates/omnid/src/ui/space-mono-OFL.txt`; `android/app/src/main/assets/licenses/`).

### Hierarchy
- **Headline** (Space Mono 700, 1.35rem / 21.6px, 1.3, -0.04em): the wordmark (at line-height 1), Explore group heads ("Onion sites") and "Add an address". On Android: the action-bar wordmark (21.6sp, -0.04), the start screen's title, and the browser's error head (both 21.6, -0.04).
- **Title** (Space Mono 700, 1.125rem / 18px, 1.35, -0.02em): empty-state heads ("Nothing matches …"). **Input** is the same size in Regular and sets the search line's text.
- **Title-small** (Space Mono 700, 0.9375rem / 15px, 1.45, -0.02em): result titles and Explore site names (at 1.4).
- **Body** (sans 400, 0.9375rem / 15px, 1.5): snippets (at most 70ch, clamped to three lines), router explanations, the crawl detail, empty-state paragraphs and the Add copy. On Android: the start screen's detail and the browser's message, at 15sp.
- **Label** (Space Mono 700, 0.78125rem / 12.5px, 1, 0.04em, capitals): chips, buttons, text buttons, router names in the trust panel and the files disclosure.
- **Meta** and **Address** (Space Mono 400, 0.78125rem / 12.5px; 1.5 and 1.4): the summary line, meta lines and counts (meta, with tabular figures), submit results, the Add line's input, and every address (one line, ellipsised).
- **State** (Space Mono 400, 0.6875rem / 11px, 1.5, 0.06em, capitals): the trust strip.
- **Badge** (Space Mono 700, 0.6875rem / 11px, 1, 0.06em, capitals): route badges, NOT ENCRYPTED, state words in the trust panel, and small buttons (at 0.04em). The browser's kind badge is its 11sp native form.

Long addresses are shortened, never cut blind. Onion and `.b32.i2p` hosts keep their first eight and last five characters (`2gzyxa5i…53wid.onion`). Hyphanet keys keep the key type, the key's first eight characters, and the site's own name and path (`USK@62H8KFSZ…/freenet-first-steps/24`). Magnets show `btih:` and the first twelve hex characters. A page with no title, or whose title is only an address, is titled by its short address; a Hyphanet page is titled by its site's name and path. The browser's address line and tab rows use the same short form.

The native browser chrome uses the same steps: 12.5sp for the address and find fields, the tab count, the find count and tab-row addresses, and 15sp for tab titles.

### Named Rules
**The Name Rule.** The product is written OMNId wherever people read it, OMN in capitals and the d lower case: in the wordmark, the launcher label, notifications and running copy ("OMNId crawls it next"). `omnid` in lower case is only the command, the package and code.

**The Two Voices Rule.** Space Mono sets everything that heads, labels, counts or addresses, including one-line status such as the summary and submit results. The platform sans sets paragraphs and nothing else.

**The Capitals Rule.** Capitals are for state and controls only: router names and states, the crawler's word, badges, chips, buttons, NOT ENCRYPTED, and the tab list's count. Titles, heads, addresses and paragraphs keep their own case. Capitals are tracked 0.04em at 12.5px and 0.06em at 11px.

**The Five Sizes Rule.** Every piece of text uses one of five sizes: 11, 12.5, 15, 18 and 21.6px (sp natively). The upper four are 1.2 apart, and 11 exists for capitals. A sixth size is a bug.

## Layout

The page is one centred column, at most 46rem (736px) wide, with a 1rem gutter that grows to the safe-area inset on notched screens and 3rem of space at the foot. From top to bottom: the header (the wordmark outside the app, then the trust strip), the 52px search line, the chip row 0.9rem below, the address row when an address is typed, a one-line summary, then the results or Explore, then "Add an address" set 2.5rem below behind a solid rule. Inside the Android app (`?embedded`) the web wordmark hides and the header's top tightens to 0.5rem, because the native action bar already carries the mark and the name on the 16dp keyline.

Spacing steps: hair (0.35rem) between a title, its snippet and its routes. Tight (0.5rem) between chips and between inline buttons. Row (0.7rem) as the vertical padding of router and site rows. Snug (0.75rem) inside the header and the address row. Stack (0.9rem) from the search line to the chips, the address row and the summary. Gutter (1rem) for page edges and the top of each result. Group (1.5rem) between Explore groups. Section (2.5rem) before the Add section. Items in a list are separated by rules, not by extra space.

Phone first. Below 720px the trust strip fills the header row and wraps its routers onto a second line when it needs to, and the chip row scrolls sideways, bleeding to both screen edges with its scrollbar hidden. From 720px the header gains a 2rem top, the strip shrinks to its content, and the chips wrap in place.

On Android the windows run edge to edge, with the system-bar and keyboard insets applied as padding. The browser toolbar holds a 48dp back button, the flexible 48dp address line (2dp margins each side), then the 48dp tab count and the 48dp menu, with 6dp vertical and 2dp horizontal padding. A 2dp progress line sits under it. The find bar replaces the toolbar's row with the find line, then 48dp previous, next and close buttons. The tab list opens with a header row (the count in capitals at a 14dp inset, then a 48dp new-tab button), followed by rows inset 8dp from the edges. The start screen is a centred column with 40dp padding: the 88dp launcher icon, the title 20dp below it, the detail 6dp below that, a 160 by 4dp progress bar 24dp lower, and on failure a Try again button 16dp under it.

### Named Rules
**The 48 Floor Rule.** Every touch target is at least 48px (48dp). When the visual is smaller, the hit area grows and the shape does not. A 40px chip's input reaches 4px above and below it. 48px icon buttons sit 12px into the margin so their glyphs align with the text edge. Text buttons are 48px tall and pulled 0.75rem into the right margin so the word ends on the column edge.

## Elevation & Depth

The system is flat paper. Nothing casts a shadow, there are no tonal cards, and depth is not a device it uses. The action bar sits at 0dp elevation on the shared ground. The trust panel opens beneath the strip as more ruled lines (a 200ms fade and 6px drop), not as a floating layer. The only second tone is `surface-2`, behind a hovered icon button and in skeleton bars. The platform's own popups (the overflow menu and dialogs) still bring the platform's elevation; they have not been restyled.

### Shadow Vocabulary
Box-shadow appears only as a drawing tool, never as light:
- **Focus line** (`box-shadow: inset 0 -2px 0 var(--glow)`): with the field's 2px bottom rule turned `glow`, it thickens a focused text line to 4px.
- **Hollow square** (`box-shadow: inset 0 0 0 1.5px var(--net)`, or `var(--off)` for a router that is not running): draws an 8px square as an outline. A chip's network square is hollow while the chip is off.
- **Ground frame** (`box-shadow: inset 0 0 0 1px var(--ground)`): a 1px ground-coloured edge on a selected chip's filled network square, so every hue stays visible on the ink block.

### Named Rules
**The Flat Paper Rule.** Surfaces are paper and nothing floats. Shadows exist only to draw lines and hollow squares, status squares carry no halo, and no element glows.

## Shapes

There are no corners. Every field, button, chip, badge, block, frame, skeleton bar and status dot is square (border-radius 0; text fields set it explicitly so platform defaults cannot round them). On Android, `bg_tab_count`, `bg_tab_current` and `bg_address` are plain rectangles. Dots are 8px squares: filled for a state or a network, hollow for "not running" or a chip that is off.

Rules replace boxes, in a small vocabulary:
- **Solid 1px `ink-2`**: opens a section. It runs above and below the trust strip and the typed address row, under the open trust panel, above an Explore list, and above the Add section.
- **Dashed 1px `ink-2`**: between results, above an empty state and above skeleton rows. On Android it is a 1dp dash (3dp on, 3dp off) inset 8dp, between tab rows and bookmark rows.
- **Dashed 1px `muted`**: between Explore sites and between router rows in the trust panel.
- **Solid 2px `ink`**: under every text field (the search line, the Add line, and the browser's address and find lines) in place of a field box.

The only outlined shapes are controls: buttons (1px `ink-2`), chips while off (1px dashed `ink-2`), badges (1px in their hue) and the tab count (a 1.75dp `ink` square). Content (results, sites, routers, tabs) is never boxed. The one filled block around content is the ink block of selection.

Icons are drawn, never glyphs or an icon font. They are outline strokes on a 24-unit grid: 1.75 stroke, square caps, miter joins, no fills. At 20px (web) and 24dp (Android) they keep the 1.75 stroke. 16px icons inside buttons and chips and 12px badge icons step up to 2.25. The overflow icon's dots are 2.75-stroke squares. The Android vectors repeat the web's paths and stroke and are tinted `ink`. Curves inside glyphs (the search ring, the lock's shackle, the reload arc) and the brand's rings are drawing, not corners.

### Named Rules
**The No Corners Rule.** border-radius is 0 everywhere. A rounded field, pill, circle dot or rounded button is a bug. The platform's dialogs, menus, borderless ripples and the launcher-mask icon on the start screen are known exceptions that have not been restyled yet; they are not a licence.

**The Rules Not Boxes Rule.** Groups are separated by lines, not enclosed. If something needs a border, it is a control. If it is content, it gets a rule above or below it.

## Components

### Buttons
Square, outlined and in capitals. They become an ink block under the finger.
- **Shape:** square (0px), at least 48px tall, padding 0 1.1rem, Space Mono Bold 12.5px in capitals at 0.04em, a 16px icon (2.25 stroke) 0.45rem before the label.
- **Default:** transparent, with a 1px `ink-2` outline and `ink` text (SHOW MORE, PAUSE / RESUME, ADD TO INDEX, GET I2PD).
- **Primary:** `accent` fill and border with `accent-ink` text (+ ADD, OPEN). One per view.
- **Hover / Focus:** both kinds become the ink block (`ink` fill and border, `ground` text) over 160ms. Focus is a 2px `glow` outline 2px out, on every control.
- **Small:** the same button with an 11px label and 0 0.9rem padding, still 48px tall. It serves in-context actions such as SHOW 12 RESULTS IN I2P and SEARCH EVERY NETWORK.
- **Text:** no fill or border; `accent` label in capitals. Used for SEE ALL, BACK TO EXPLORE, and the files disclosure (N FILES · SIZE with a 16px chevron that turns 180° when open).
- **Icon:** a 48px square with a `muted` 20px glyph. Hover gives a square `surface-2` wash and an `ink` glyph. Copy confirms by turning into an `ok` check for 1.4s.
- **Native:** the start screen's Try again is a square `accent` block with `accent-ink` Space Mono Bold in capitals (0.04), an `ink` ripple, at least 48dp tall with 20dp side padding. Toolbar buttons are 48dp squares with 24dp `ink` icons. A disabled one (back with no history) drops its icon to 80/255 alpha. Menu text is `ink`, at 38% when disabled.

### Chips
- **Style:** square, 40px tall (a 48px target), padding 0 0.8rem 0 0.7rem, Space Mono Bold 12.5px in capitals with the count in Regular. Each chip has an 8px network square drawn hollow in the network's hue.
- **Off:** transparent, with a 1px dashed `ink-2` outline and `ink-2` text.
- **On:** the ink block (`ink` fill, `ground` text), a 16px check (2.25 stroke) before the square, and the square filled in its hue inside a 1px ground frame. A selected chip whose network has no results is a `muted` block instead.
- **Behaviour:** multi-select network filters, all on by default. The networks with the most results come first (with nothing typed, the most pages in the index). Ties keep the usual order: onion, web, torrents, I2P, Hyphanet. The row scrolls back to its start when the order changes. Toggling a chip never reorders, because the counts do not depend on the selection.

### Cards / Containers
- **Corner Style:** there are no cards. Results, Explore sites, routers and tabs are rows on the ground, separated by rules (see Shapes).
- **Background:** the ground only. The trust panel, the address row and the Add section are bands between rules, not containers.
- **Shadow Strategy:** none; see Elevation & Depth.
- **Empty states:** a dashed rule above, 1.5rem of padding, a Title head in `ink`, a Body paragraph in `ink-2`, and small buttons that widen the search.

### Inputs / Fields
- **Style:** text is typed on a line. The search line is 52px tall: transparent, with no box, a 2px `ink` bottom rule, 18px Space Mono Regular at -0.02em, a 20px `muted` search icon at the left edge, and a 48px clear button (×) hanging 0.75rem into the right margin. The Add line is 48px with 12.5px mono text beside the primary + ADD button (0.75rem gap, aligned to the line).
- **Focus:** the rule turns `glow` and thickens to 4px (the focus line, over 200ms). The caret is `glow` everywhere.
- **Error / Confirmation:** a 12.5px mono line under the field, in `bad` for errors and `ok` for confirmations. The field itself does not change.
- **Typed address:** when the search line holds an onion, `.i2p`, magnet, Hyphanet or web address, a ruled row (solid `ink-2` above and below) appears under the chips with its badge, the short address, OPEN (primary, where this browser can open it) and ADD TO INDEX.
- **Native:** the browser's address and find fields sit on the same 2dp `ink` line (`bg_address`) in Space Mono Regular, `ink` text with `muted` hints. Text that does not fit fades out over 20dp at the edge.

### Navigation
- **Web header:** outside the app, the wordmark (the flat ring mark at 28px, 0.6rem gap, "OMNId" as a Headline) sits at the left, and the trust strip fills the rest. Inside the app the wordmark hides.
- **Android action bar:** `ground` at 0dp elevation. It carries the brand view (the 30dp mark, a 12dp gap, then "OMNId" in Space Mono Bold 21.6sp at -0.04 in `ink`), the tab count while any tab is open, and the overflow menu (crawl on battery and mobile data, Hyphanet node, Stop OMNId).
- **Mobile treatment:** there is no tab bar or drawer. The search page is home, results open the private browser, and system Back returns.

### Trust Strip
The page's signature: honest state, before anything else. It is a 48px line ruled above and below in 1px `ink-2`, with no box, set in 11px Space Mono capitals at 0.06em in `ink-2`. It holds the routers (an 8px square and a name: TOR, TORRENTS, I2P, HYPHANET), then the crawler's word in `muted`, then a 16px `muted` chevron. The word is PAUSED, CRAWLING while anything is queued or loading, and IDLE otherwise, so it does not flicker between pages. At larger text sizes the routers wrap onto a second line and the strip grows; no name or word is ever cut.

Router states: a filled `ok` square means ready. A filled `warn` square that breathes (to 0.35 opacity over 1.6s) means connecting. A hollow `off` square means not running. A filled `off` square, with the name in `muted`, means off. If the daemon does not answer, the strip says "OMNId is not answering" beside a hollow square. Tapping the strip opens the trust panel. The strip's rules turn `accent` on hover and while the panel is open, and the chevron turns 180°. Screen readers hear every state in words.

The trust panel lists every router, Web included: its square, its name in Label (`ink`), its state word in Badge in the state colour (`muted` for not running and off), and one plain sentence in Body that says where the router is ("on this device", or the other machine's address) without ports, proxy names or node counts. Router rows are separated by dashed `muted` rules. The crawl row closes the panel: a sentence (paused and why, or crawling, or waiting; pages indexed; addresses found) and a PAUSE or RESUME button. In the app, the I2P row adds GET I2PD or OPEN I2PD.

### Result
One anatomy at one scale, below a dashed `ink-2` rule, with padding 1rem 0 0.95rem.
- **Title** (Title-small, `ink`): opens the private browser through the route that stays inside an overlay network where there is one, then encrypted web, then plain web. Matched terms take the Periwinkle Wash.
- **Snippet** (Body, `ink-2`): at most 70ch and three lines, with matched terms in the wash.
- **Routes:** each on its own 48px line, 0.6rem apart: the route badge, the short address (Address, `ink-2`, `accent` on hover when it is a link), NOT ENCRYPTED in `warn` Badge type for plain-HTTP web routes, and a copy icon button. Onion and `.i2p` addresses are plain text outside the app.
- **Route badge:** a 1px outline in the network's hue, its label in the hue (ONION, WEB, TORRENT, I2P, HYPHANET) in Badge type, and a 12px lock, open lock or magnet icon (2.25 stroke). Padding is 0.2rem 0.45rem 0.2rem 0.35rem.
- **Meta** (Meta, `muted`): language, route count and seen dates, joined by " · ". Torrents add the files disclosure, whose list is mono and at most fifty lines.
- **Loading:** after 150ms with no answer, three skeleton rows appear: square `surface-2` bars that blink in two steps (to 0.45 opacity, 1.2s). There is no spinner.

**The Highlighter Rule.** An unread title carries the highlighter: a `hi` underline 0.5em thick, offset -0.35em so it sits behind the lower half of the letters, with skip-ink off. On hover the stroke turns solid `accent`. Once the link is visited the stroke is gone and the title drops to `ink-2`. Explore site names carry the same stroke.

**The In-Place Rule.** Results replace each other where they stand. Rows already showing stay still, only new rows fade in (from 0.4 opacity, 4px lower, over 220ms), there is no spinner, and skeleton rows appear only when the index takes more than 150ms.

### Explore
What the index holds when nothing is typed. Each network with pages gets a group, in the chips' order. The head is a Headline ("Onion sites", "On the web", "Torrents", "I2P sites", "Hyphanet sites") with the count in `muted` Meta and a SEE ALL text button. A solid `ink-2` rule opens the list. Explore lists sites, not pages: one row per host or Hyphanet site, five per network, separated by dashed `muted` rules. Each row has a filled 8px network square spanning two lines, the site name in Title-small with the highlighter, and the short address in `muted` Address type (left out when the name already is the address). Row padding is 0.7rem 0.

### Browser Toolbar
The Android browser's chrome: native, on `ground`, speaking the page's type.
- **Row:** back, the address line, the tab count, and the menu. The address line is the 2dp `ink` rule holding the kind badge, the address in Space Mono Regular `ink`, and reload (stop while loading) at its right end. Tapping anywhere on the line focuses the address and selects all of it.
- **Kind badge:** the route badge's native form. It is 11sp Space Mono Bold in capitals at 0.06, with a 1dp outline and a 12dp lock in the same colour, and 4/5dp padding. It reads ONION in violet, I2P in teal and HYPHANET in blue; TOR in green over HTTPS; and "TOR · NOT ENCRYPTED" in `warn` with an open lock over plain HTTP.
- **Progress:** a 2dp `glow` line on a `ground` track under the toolbar.
- **Find bar:** the find field on the same line, its "1 of 6" count in `muted` mono, then previous, next and close.
- **Opening:** the browser opens with a 240ms decelerating scale from 0.94 (pivot 50% / 60%) and a fade, while the search page behind it fades to 0.6.
- **Reload:** a 48dp square at the line's right end, like every other toolbar button.

### Tab List
- **Tab count:** a 22dp square with a 1.75dp `ink` outline around the number in Space Mono Bold, inside a 48dp target. It appears in the browser toolbar and, while tabs are open, in the search page's action bar.
- **Rows:** inset 8dp from the edges, at least 60dp tall, padding 6dp 2dp 6dp 12dp. The page title is in Space Mono Bold (-0.02, one line, ellipsised). Below it, 4dp down, the kind badge sits 8dp before the short address in `muted` mono. A 48dp × closes the tab. Rows are separated by the dashed rule. Bookmarks reuse the same rows and rule.
- **Current tab:** the ink block. Its title, badge, address and × all take the `ground` colour.

### Brand Mark
The icon's four interlocking rings, drawn flat. The web header draws them as an inline SVG stroked in `accent` (stroke 5.48 on a 49-unit box) in both schemes, with no tile and no glow. The Android action bar uses the same rings as a 30dp `accent` vector in both schemes. The launcher icon sets the rings on `ground`.

### Load-Failure Page
The browser's error page is always the dark scheme, because the engine reports a light scheme to resist fingerprinting. It has `ground` behind and 3rem 1.5rem of padding. The head is 21.6px Space Mono Bold (embedded as a data URL so nothing is fetched). Below it are the reason in 15px sans `ink-2`, the full address and the error code in 12.5px `muted`.

## Do's and Don'ts

### Do:
- **Do** mark selection and "on" with the ink block: `ink` fill and `ground` text, for chips, the shown tab, selected text and hovered buttons.
- **Do** keep a network's hue to its 8px square, its outlined badge (a 1px hue outline and 11px bold capitals in the hue) and the browser's kind badge.
- **Do** separate content with rules: solid 1px `ink-2` to open a section, dashed 1px `ink-2` between results, dashed 1px `muted` between Explore sites and router rows, and a 2px `ink` line under every text field.
- **Do** keep every corner square (border-radius 0) on fields, buttons, chips, badges, blocks, frames and dots.
- **Do** set heads, labels, counts, addresses and status lines in Space Mono, and paragraphs in the platform sans.
- **Do** keep text on the five sizes (11, 12.5, 15, 18, 21.6), with capitals tracked 0.04em at 12.5px and 0.06em at 11px.
- **Do** make every touch target at least 48px (48dp). When the visual is smaller, extend the hit area, not the shape.
- **Do** paint the window, system bars, action bar, browser toolbar and page with the same `ground`, and give a new colour the same value in app.css and colors.xml for each scheme.
- **Do** draw icons on a 24-unit grid with a 1.75 stroke, square caps, miter joins and no fill, stepping up to 2.25 only for 16px icons in buttons and chips and 12px badge icons.
- **Do** shorten onion and `.b32.i2p` hosts to their first eight and last five characters, and Hyphanet keys to the type, the key's first eight characters and the site's name and path.
- **Do** keep motion short and decelerating (cubic-bezier(0.16, 1, 0.3, 1), 160 to 240ms), update results in place, and turn all motion off under reduced motion.
- **Do** serve any font from the daemon's own origin, as Space Mono is (`font-src 'self'`).

### Don't:
- **Don't** round a corner, draw a pill or a circle dot, or put content in a card or a tonal box.
- **Don't** fill a chip, tint a row or colour a rule with a network hue.
- **Don't** add a shadow, glow or halo to anything. Box-shadow only draws lines and hollow squares.
- **Don't** load a font, script or image from another origin.
- **Don't** use `glow` as a fill. It marks focus, the caret and load progress only.
- **Don't** set a paragraph in Space Mono, or a head, label, address or status line in the sans.
- **Don't** set titles, heads, addresses or paragraphs in capitals.
- **Don't** add a text size off the five-step ramp.
- **Don't** show a spinner while searching.
- **Don't** give the action bar or browser toolbar a colour band or elevation of its own.

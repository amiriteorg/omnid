---
name: omnid
description: One calm, fast search across every network, with trust visible on every result.
colors:
  ground: "#0B0D1F"
  surface: "#131736"
  surface-2: "#1A1F45"
  line: "#262C57"
  ink: "#EEF2FF"
  ink-2: "#B2BBDF"
  muted: "#8690BD"
  accent: "#95AEFB"
  accent-ink: "#0B0D1F"
  glow: "#4A87FB"
  focus: "rgb(74 135 251 / 0.55)"
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
  surface-light: "#FFFFFF"
  surface-2-light: "#EDF1FB"
  line-light: "#DDE3F3"
  ink-light: "#0B0D1F"
  ink-2-light: "#3D4670"
  muted-light: "#5D668F"
  accent-light: "#2F5BE6"
  accent-ink-light: "#FFFFFF"
  glow-light: "#2F5BE6"
  focus-light: "rgb(47 91 230 / 0.45)"
  mark-light: "rgb(47 91 230 / 0.14)"
  mark-ink-light: "#0B0D1F"
  ok-light: "#128A52"
  warn-light: "#9C5D00"
  bad-light: "#C8283A"
  off-light: "#9AA2C2"
  onion-light: "#6A49DF"
  clearnet-light: "#4A5480"
  magnet-light: "#9C5D00"
  i2p-light: "#0B7F86"
  hyphanet-light: "#2A63D6"
typography:
  headline:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "1.35rem"
    fontWeight: 650
    lineHeight: 1.5
    letterSpacing: "-0.01em"
  title:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "1.125rem"
    fontWeight: 600
    lineHeight: 1.3
  body:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.5
  body-strong:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "0.9375rem"
    fontWeight: 600
    lineHeight: 1.5
  label:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "0.78125rem"
    fontWeight: 400
    lineHeight: 1.5
    fontFeature: '"tnum"'
  label-strong:
    fontFamily: 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
    fontSize: "0.78125rem"
    fontWeight: 600
    lineHeight: 1.5
  mono:
    fontFamily: 'ui-monospace, "Roboto Mono", SFMono-Regular, Menlo, monospace'
    fontSize: "0.78125rem"
    fontWeight: 400
    lineHeight: 1.4
rounded:
  mark: "4px"
  sm: "8px"
  control: "14px"
  surface: "16px"
  pill: "999px"
spacing:
  tight: "0.5rem"
  snug: "0.75rem"
  gutter: "1rem"
  group: "1.25rem"
  section: "2.5rem"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.control}"
    padding: "0 1.1rem"
    height: "48px"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.control}"
    padding: "0 1.1rem"
    height: "48px"
  button-text:
    textColor: "{colors.accent}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.control}"
    padding: "0 0.75rem"
    height: "48px"
  icon-button:
    textColor: "{colors.muted}"
    rounded: "{rounded.pill}"
    size: "48px"
  icon-button-hover:
    backgroundColor: "{colors.surface-2}"
    textColor: "{colors.ink}"
  search-field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    rounded: "{rounded.surface}"
    padding: "0 3rem 0 2.9rem"
    height: "52px"
  address-field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "0 0.9rem"
    height: "48px"
  chip:
    textColor: "{colors.muted}"
    rounded: "{rounded.pill}"
    padding: "0 0.85rem 0 0.7rem"
    height: "40px"
  chip-selected:
    textColor: "{colors.ink}"
  route-badge:
    typography: "{typography.label-strong}"
    rounded: "{rounded.pill}"
    padding: "0.15rem 0.5rem 0.15rem 0.4rem"
  trust-strip:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink-2}"
    typography: "{typography.label}"
    rounded: "{rounded.pill}"
    padding: "0.45rem 0.75rem"
    height: "48px"
  trust-panel:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.surface}"
    padding: "0.5rem 1rem 0.75rem"
  result-row:
    padding: "1rem 0 1.05rem"
  browser-toolbar:
    backgroundColor: "{colors.ground}"
    padding: "6px 2px"
  browser-address-pill:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.pill}"
    padding: "0 12px 0 14px"
    height: "44px"
---

# Design System: omnid

## Overview

**Creative North Star: "The Night Switchboard"**

omnid is one board that every network plugs into: onion sites, I2P, Hyphanet, torrents and the open web, each line labelled with where it leads and lit to show whether it is live and whether the way there is sealed. The board sits on the icon's deep navy and is lit only by the icon's own blues. The metaphor shapes structure, never imagery: there are no cables, sockets or retro hardware anywhere, and the system rejects both a themed "dark web" costume and the generic grey search app.

The feel is calm and quick, built to the bar of Arc Search, Signal and Proton. Density is phone-first and moderate: one column, hairline-separated rows, a single large search field, and a trust strip that states router health in plain words before anything else. Colour is spent on meaning. One accent carries action and selection, glow blue carries focus, and each network owns a hue that appears only as a small dot or a route badge. Depth comes from tone, not shadow.

Light and dark are one token set expressed twice, shared by the web page (`crates/omnid/src/ui/app.css`) and the Android chrome (`res/values*/colors.xml`). Dark is the brand's home: navy ground, moonlit ink. Light is cool paper with navy ink and a stronger blue, so every accent role keeps its contrast. Where the icon's pale glow would vanish on the light ground, the brand mark changes rendition instead of the palette changing hue.

**Key Characteristics:**
- One navy ground shared by the window, system bars, toolbars and page.
- One accent for action and selection; glow blue only for focus, the caret and progress.
- Network hues confined to 8px dots, route badges and the browser's kind label.
- The platform sans on four sizes 1.2 apart; mono only for addresses; tabular figures for data.
- Flat tonal surfaces with 1px hairlines; the focus ring is the only coloured shadow.
- Pills for state and filters, 14px rounded rectangles for actions, 16px corners for surfaces.
- Every touch target at least 48px (48dp).
- Short, decelerating motion; results update in place.

## Colors

An icon-derived palette: deep navy grounds, one periwinkle-to-blue accent, quiet state signals, and five network hues kept small. Each token exists in a dark value (the bare name) and a light value (the `-light` name); the scheme follows the system on both platforms.

### Primary
- **Ring Periwinkle** (`accent` #95AEFB; `accent-light` #2F5BE6): primary buttons, the selected chip's tonal fill (13% over surface), border (45% into line) and check, links, text buttons such as "See all", hover borders on the trust strip and secondary buttons, and text selection. Text on it is `accent-ink` (#0B0D1F dark, #FFFFFF light).
- **Glow Blue** (`glow` #4A87FB; `glow-light` #2F5BE6): focus outlines, focused field borders, the text caret, and the Android progress bars (the start screen and the browser's load line). It is never a fill. The direction contract gave glow blue the primary action; the build gave that to the accent and kept glow for focus, and the build is the system.
- **Focus Ring** (`focus` rgb(74 135 251 / 0.55); `focus-light` rgb(47 91 230 / 0.45)): the 4px spread ring around a focused text field.
- **Periwinkle Wash** (`mark` rgb(149 174 251 / 0.22); `mark-light` rgb(47 91 230 / 0.14)) with **Ice** ink (`mark-ink` #CFECFB; `mark-ink-light` #0B0D1F): matched query terms in titles and snippets.

### Secondary
State signals for routers, connections and submissions.
- **Signal Green** (`ok` #4FD18B; `ok-light` #128A52): a ready router, "Copied", an accepted address, and the browser's "Tor" label over HTTPS.
- **Amber Caution** (`warn` #F5B65B; `warn-light` #9C5D00): a connecting router (its dot breathes), "not encrypted" beside plain-HTTP routes, and the browser's plain-HTTP label with an open lock.
- **Coral Alarm** (`bad` #FF7A86; `bad-light` #C8283A): a refused or failed submission.
- **Unlit Slate** (`off` #59628F; `off-light` #9AA2C2): a router that is off (filled dot) or not running (hollow 1.5px ring).

### Tertiary
Network marks. Each hue identifies where a page lives and appears only as a dot, a badge or the browser's kind label.
- **Onion Violet** (`onion` #A98BFF; `onion-light` #6A49DF): onion sites.
- **Clearnet Ice** (`clearnet` #C9D6F5; `clearnet-light` #4A5480): the open web, labelled "Web".
- **Magnet Amber** (`magnet` #F5B65B; `magnet-light` #9C5D00): torrents; it shares its value with `warn`, and context (a badge with a magnet icon) separates them.
- **I2P Teal** (`i2p` #45D3D9; `i2p-light` #0B7F86): `.i2p` sites.
- **Hyphanet Blue** (`hyphanet` #6FA2FF; `hyphanet-light` #2A63D6): Hyphanet sites.

### Neutral
- **Deep Ring Navy** (`ground` #0B0D1F; light **Cool Paper** `ground-light` #F5F7FC): the one ground under everything. The launcher icon's background is the same navy.
- **Raised Navy** (`surface` #131736; `surface-light` #FFFFFF): the search and address fields, the trust strip and panel, secondary buttons, the browser's address pill.
- **Lifted Navy** (`surface-2` #1A1F45; `surface-2-light` #EDF1FB): the hover wash behind icon buttons and the travelling highlight in skeleton rows.
- **Hairline Indigo** (`line` #262C57; `line-light` #DDE3F3): every 1px border and row divider.
- **Moonlit White** (`ink` #EEF2FF; light **Navy Ink** `ink-light` #0B0D1F): titles, input text, the brand name, toolbar icons.
- **Mist Periwinkle** (`ink-2` #B2BBDF; `ink-2-light` #3D4670): snippets, explanations, addresses, visited titles.
- **Dusk Slate** (`muted` #8690BD; `muted-light` #5D668F): placeholders, meta lines, counts, resting icons, unselected chip text.

### Named Rules
**The Dots-and-Badges Rule.** A network's hue appears only as an 8px dot, a route badge (the hue at 14% behind text in the hue) or the browser's kind label. It is never a chip fill, a row tint, a button or a border.

**The One Accent Rule.** Selection looks the same on every network: accent at 13% over surface, an accent-tinted border and an accent check. The `glow` token marks focus, the caret and progress, and nothing else.

**The Shared Ground Rule.** The window, status bar, navigation bar, action bar, browser toolbar and page all paint `ground`, so the app reads as one surface. app.css and colors.xml carry the same value for the same name in each scheme; a token added on one side takes the other side's value.

## Typography

**Body Font:** system-ui (with -apple-system, "Segoe UI", Roboto, sans-serif); on Android the platform `sans-serif`, with `sans-serif-medium` for the brand name and labels.
**Label/Mono Font:** ui-monospace (with "Roboto Mono", SFMono-Regular, Menlo, monospace), for addresses only.

**Character:** The phone's own sans: native, instant, nothing to download (the page's content policy loads nothing from outside its origin). Weight carries the hierarchy, 650 for section heads, 600 for titles and actions, 500 for chips, 400 for reading, while mono is kept for the strings people compare character by character.

### Hierarchy
- **Headline** (650, 1.35rem / 21.6px, -0.01em): section heads such as "Onion sites" and "Add an address", and the brand name (which runs +0.01em; 21.6sp medium on Android).
- **Title** (600, 1.125rem / 18px, 1.3): result titles and empty-state heads. The search field's text uses this size at weight 400.
- **Body** (400, 0.9375rem / 15px, 1.5): snippets (at most 70ch, clamped to three lines), router explanations, inputs, the browser's address. **Body-strong** (600) sets buttons, router names and Explore site names; chips use the body size at 500.
- **Label** (400, 0.78125rem / 12.5px): the trust strip, the result summary, meta lines, file lists; tabular figures wherever numbers sit. **Label-strong** (600) sets route badges; the browser's kind label is its 12sp native form.
- **Mono** (400, 0.78125rem / 12.5px, 1.4): addresses in routes and Explore rows, ellipsised on one line, with onion and `.b32.i2p` hosts shortened to their first eight and last five characters and Hyphanet keys to their type, the key's first eight characters and the site's own name and path (`USK@62H8KFSZ…/freenet-first-steps/24`).

### Named Rules
**The Name Rule.** The product is written OMNId wherever people read it, OMN in capitals and the d lower case, in the brand, the launcher label, notifications and running copy ("OMNId crawls it next"). `omnid` in lower case is only the command, the package and code.

**The Four Sizes Rule.** Every piece of text sits on one of four sizes 1.2 apart: 12.5, 15, 18 and 21.6px, the same values in sp natively. A fifth size is a bug.

**The Mono-Means-Address Rule.** Monospace sets addresses and hashes only, never labels, counts or headings; numbers in the sans use tabular figures instead.

## Layout

The page is one centred column, at most 46rem (736px) wide, with a 1rem gutter that grows to the safe-area inset on notched screens. Top to bottom: the header (the brand outside the app, then the trust strip), the 52px search field, the row of network chips, a one-line summary, the results or Explore, then "Add an address" set 2.5rem below. Inside the Android app (`?embedded`) the web brand hides and the header tightens to a 0.5rem top, because the native action bar already carries the mark and the name on the 16dp keyline.

Spacing steps are tight (0.5rem) between chips and inline controls, snug (0.75rem) inside the strip and between header items, gutter (1rem) for page edges and the top of each result, group (1.25rem) between Explore groups, and section (2.5rem) before the Add section. Items within a list are divided by hairlines, not by extra space.

Phone first. Below 720px the trust strip fills the header row and the chip row scrolls sideways, bleeding to both screen edges with its scrollbar hidden. From 720px the header gains a 2rem top, the strip shrinks to its content, and the chips wrap in place.

On Android the windows run edge to edge with the system-bar and keyboard insets applied as padding. The browser toolbar lays out a 48dp back button, a flexible 48dp address pill, then 48dp reload and menu buttons, with 2dp gaps and 6dp vertical padding, and a 2dp progress line under it. The start screen is a centred column with 40dp padding: the 88dp launcher icon, the title 20dp below it, the detail 6dp below that, a 160 by 4dp progress bar 24dp lower, and a retry button 16dp under it on failure.

### Named Rules
**The 48 Floor Rule.** Every touch target is at least 48px (48dp). When the visual is smaller, the hit area grows and the shape does not: a 40px chip's input reaches 4px above and below it, and 48px icon buttons sit 12px into the margin so their glyphs align with the text edge.

## Elevation & Depth

The system is flat. Depth is tonal, ground under surface under surface-2, with 1px `line` hairlines at the edges, and nothing casts a shadow at rest. The action bar is at 0dp elevation and shares the ground. Colour glows exist only inside the icon art; UI elements and status dots carry none. Should a layer ever float over content (none does today), it takes a soft neutral shadow, never a coloured one.

### Shadow Vocabulary
- **Focus ring** (`box-shadow: 0 0 0 4px var(--focus)`): around a focused search or address field, with the border turning `glow`. A spread without blur, so it reads as a ring.
- **Hollow dot** (`box-shadow: inset 0 0 0 1.5px var(--net)`): draws an unselected chip's network dot as a ring; it fills when the chip is selected.

### Named Rules
**The Flat Night Rule.** Surfaces are flat at rest and separated by tone and 1px hairlines. The focus ring is the only coloured shadow, status dots carry no halos, and the glow belongs to the icon art alone.

## Shapes

The form language is soft and consistent. Actions are gently rounded rectangles (14px): buttons, the address field, text buttons. Surfaces take slightly rounder corners (16px): the search field and the trust panel. Anything that shows or sets state is a full pill (999px): network chips, the trust strip, route badges, and the browser's address pill (24dp on a 48dp height). Icon buttons are 48px circles. Small details: matched-term highlights at 4px, the focus outline and the web brand image at 8px, skeleton bars at 6px. Dots are 8px circles.

Icons are drawn, never glyphs or an icon font: outline strokes on a 24-unit grid, 1.75 stroke, round caps and joins, no fills. At 20px they keep the 1.75 stroke; smaller icons step it up to hold their weight (14px badge icons at 2, the 16px chip check at 2.25). The Android vectors repeat the web paths and stroke and are tinted `ink`. The brand mark is the icon's four interlocking rings.

### Named Rules
**The Pill Carries State Rule.** Fully rounded shapes are kept for things that show or set state: chips, the trust strip, route badges and the address pill. Actions are 14px rounded rectangles and surfaces 16px; a pill-shaped button breaks the language.

## Components

### Buttons
Quiet and solid: a single accent fill for the one thing to do, everything else on surface or bare.
- **Shape:** gently rounded rectangle (14px), 48px tall, 600-weight body text.
- **Primary:** `accent` fill with `accent-ink` text, 0 1.1rem padding, an optional 18px leading icon 0.4rem from the label ("+ Add").
- **Hover / Focus:** the primary fill mixes 12% toward `ink` over 160ms; a secondary button's border turns `accent`. Focus is a 2px `glow` outline 2px out, on every control.
- **Secondary:** `surface` fill, 1px `line` border, `ink` text ("Show more", "Pause"); a small variant at the label size serves in-context actions such as "Show 12 results in I2P".
- **Text:** no fill or border, `accent` text, 0 0.75rem padding, pulled 0.75rem into the right margin so the word ends on the column edge ("See all", "Back to Explore").
- **Icon:** 48px circle, `muted` 20px glyph; a `surface-2` wash and `ink` glyph on hover. Copy confirms by turning into a green check for 1.4s.
- **Native:** the start screen's Retry is a platform button tinted `accent` with `accent-ink` text in sentence case.

### Chips
- **Style:** pill, 40px tall, transparent with a 1px `line` border, `muted` 15px/500 label, an 8px network dot drawn as a hollow ring, and the count in tabular figures.
- **State:** selected chips take `accent` at 13% over `surface`, a border of `accent` 45% into `line`, `ink` text, a filled dot and a 16px accent check before the dot. A selected chip with no results keeps `muted` text. The chips are multi-select filters, all on by default.
- **Order:** the networks with the most results come first (with nothing typed, the most pages in the index), ties in the usual order (onion, web, torrents, I2P, Hyphanet), and the row scrolls back to its start when the order changes, so the networks that have anything are always on screen. Toggling a chip never reorders: the counts do not depend on the selection.

### Cards / Containers
- **Corner Style:** there are no cards. Results and Explore sites are rows on the ground divided by 1px `line` hairlines.
- **Background:** the one container is the trust panel: `surface` fill, 1px `line` border, 16px corners, 0.5rem 1rem 0.75rem padding. It opens with a 200ms fade and a 6px drop.
- **Shadow Strategy:** none; see Elevation & Depth.
- **Empty states:** a hairline row with a title-size head, `ink-2` body, and small secondary buttons that widen the search.

### Inputs / Fields
- **Style:** the search field is 52px tall with 16px corners, `surface` fill, 1px `line` border, title-size text, a 20px `muted` search icon inset 1rem, and a 48px clear button at the right end. The address field is 48px with 14px corners and body text, set beside the primary Add button with a 0.5rem gap.
- **Focus:** the border turns `glow` and the 4px focus ring appears (200ms); the caret is `glow`.
- **Error / Disabled:** errors appear as a label-size line under the field in `bad`, confirmations in `ok`; the field itself does not change colour. A disabled native toolbar button drops its icon to 80/255 alpha.

### Navigation
- **Web header:** the brand (28px rounded launcher icon and "OMNId" in headline) at the left, the trust strip filling the rest; the brand hides inside the app.
- **Android action bar:** `ground` background at 0dp elevation, carrying a custom brand view (the 30dp mark, 12dp gap, "OMNId" at 21sp medium in `ink`) and an overflow menu.
- **Mobile treatment:** there is no tab bar or drawer. The search page is home; results open the private browser, and system Back returns.

### Trust Strip
The signature of the page: honest state before anything else. A 48px pill on `surface` with a `line` border holds a row of routers (an 8px dot and a name: Tor, Torrents, I2P, Hyphanet), then the crawler's word in `muted`, then a 16px chevron. The word is Paused, Crawling while anything is queued or loading, and Idle otherwise, so it does not flicker between pages. At larger text sizes the router names wrap onto a second line and the pill grows; no name or word is ever cut. A filled `ok` dot means ready; a filled `warn` dot that breathes (1.6s) means connecting; a hollow `off` ring means not running; a filled `off` dot means off. Tapping it opens the trust panel, where each router gets its dot, a bold name, its state word in the state colour, and one plain sentence that says where the router is ("on this device", or the other machine's address) without ports, proxy names or node counts, and the crawl line offers Pause or Resume. The border turns `accent` on hover and while open, and the chevron turns 180°. Screen readers hear every state in words.

### Result Row
One anatomy at one scale. The title (title size, `ink`, `ink-2` once visited, an accent underline on hover) opens the private browser. The snippet follows in `ink-2`, at most 70ch and three lines, with matched terms in the periwinkle wash. Each route is its own 48px line: a route badge (network hue at 14% behind 12.5px/600 hue text, with a 14px lock, open lock or magnet icon), the mono address in `ink-2`, "not encrypted" in `warn` for plain-HTTP web routes, and a copy icon button. A label-size meta line closes the row (language, route count, seen dates); torrents add an accent "N files" disclosure. Onion and `.i2p` addresses stay plain text outside the app. A page with no title, or whose title is only an address, is titled by its short address (a Hyphanet page by its site's name and path). Explore rows are the compact form: a filled 8px network dot spanning two lines, the site name in body-strong and the address in mono `muted` (once, when the site has no name but its address), under a headline, a count and a "See all" text button. Explore lists sites, not pages: one row per host or Hyphanet site, five per network, and the groups follow the chips' order.

**The In-Place Rule.** Results replace each other where they stand: rows already showing stay still, only new rows fade in (from 0.4 opacity, 4px lower, over 220ms), there is no spinner, and skeleton rows appear only when the index takes more than 150ms.

### Browser Toolbar
The Android browser's chrome, native and on `ground`. Back, the address pill, reload (stop while loading) and more, with 24dp `ink` icons. The pill is `surface` with a 1dp `line` stroke and 24dp corners, 48dp tall: a kind label first (12sp medium in the network or state colour with a 16dp tinted lock: violet "Onion", teal "I2P", green "Tor" over HTTPS, `warn` with an open lock over plain HTTP), then the address in 15sp `ink`. A 2dp `glow` line shows load progress. The browser opens with a 240ms decelerating scale from 0.94 and a fade, while the search page behind fades to 0.6. Its load-failure page is always the dark scheme, because the engine reports a light scheme to resist fingerprinting.

### Brand Mark
The four rings, rendered per scheme. On the dark ground the toolbar shows the icon's own glowing rings (a cropped raster); on the light ground it shows the same rings as a 30dp `accent` vector, because the pale glow would not read on cool paper. The web header uses the rounded launcher icon in both schemes, and the launcher icon sets the rings on `ground`.

## Do's and Don'ts

### Do:
- **Do** keep a network's hue to its 8px dot, its route badge (the hue at 14% behind 12.5px/600 hue text) and the browser's kind label.
- **Do** show selection with the one accent treatment on every network: accent at 13% over surface, a border of accent 45% into line, ink text and a 16px accent check.
- **Do** make every touch target at least 48px (48dp); when the visual is smaller, as with the 40px chips, extend the hit area instead of the shape.
- **Do** paint the window, system bars, action bar, browser toolbar and page with the same `ground`, and give a new colour the same value in app.css and colors.xml for each scheme.
- **Do** set addresses in mono at 12.5px on one ellipsised line, with onion hosts shortened to both ends, and set counts, timings and dates in tabular figures.
- **Do** divide list items with 1px `line` hairlines and let tone (ground, surface, surface-2) carry depth.
- **Do** draw icons on a 24-unit grid with a 1.75 stroke, round caps and joins and no fill, stepping the stroke up only below 20px.
- **Do** keep motion short and decelerating (cubic-bezier(0.16, 1, 0.3, 1), 160 to 240ms), update results in place, and turn all motion off under reduced motion.
- **Do** use the accent vector rings for the brand mark on the light ground and the glowing raster on the dark ground.

### Don't:
- **Don't** fill a chip, tint a row, or colour a button or border with a network hue.
- **Don't** add coloured glow shadows or halos to UI elements or status dots; the 4px focus ring is the only coloured shadow, and the glow belongs to the icon art.
- **Don't** put results or Explore sites in cards.
- **Don't** add a text size off the four-step ramp, or a web font: the page's content policy loads nothing from outside its own origin.
- **Don't** use mono for anything but addresses and hashes.
- **Don't** use the `glow` token as a fill; it marks focus, the caret and progress only.
- **Don't** show a spinner while searching.
- **Don't** give the action bar or browser toolbar a colour band or elevation of its own.

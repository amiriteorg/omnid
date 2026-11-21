"use strict";

// Everything shown here came from crawled pages. It is only ever inserted as
// text (append/textContent), never as markup; matches are marked by wrapping
// text nodes in <mark>.

const $ = (id) => document.getElementById(id);

// Inside the Android app the window has its own title bar, and omnid's
// browser opens onion and .i2p addresses over Tor and I2P.
const EMBEDDED = new URLSearchParams(location.search).has("embedded");
if (EMBEDDED) document.documentElement.classList.add("embedded");
// Languages to rank first, from the app's Settings ("de,en"); the browser's
// Accept-Language otherwise.
const LANG = new URLSearchParams(location.search).get("lang");
const ALL_NETWORKS = 5;
const PAGE = 20;
const NETWORK_NAMES = { onion: "Onion", clearnet: "Web", magnet: "Torrent", i2p: "I2P", hyphanet: "Hyphanet" };

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.filter((c) => c !== null && c !== undefined && c !== false));
  return node;
}

// Drawn icons, one stroke weight. Paths only; nothing from the network.
const ICONS = {
  search: ["M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z", "M20 20l-4-4"],
  x: ["M6 6l12 12", "M18 6L6 18"],
  copy: ["M9 9h10v11H9z", "M5 15V4h10"],
  check: ["M5 12.5l4.5 4.5L19 7.5"],
  lock: ["M6 11h12v9H6z", "M8.5 11V8a3.5 3.5 0 0 1 7 0v3"],
  unlock: ["M6 11h12v9H6z", "M8.5 11V8a3.5 3.5 0 0 1 6.8-1.2"],
  magnet: ["M6 4v8a6 6 0 0 0 12 0V4", "M6 8h4", "M14 8h4"],
  plus: ["M12 5v14", "M5 12h14"],
  pause: ["M9 6v12", "M15 6v12"],
  play: ["M8 5.5v13l10-6.5z"],
  arrow: ["M7 17L17 7", "M9 7h8v8"],
  chevron: ["M6 9l6 6 6-6"],
};

function icon(name) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "icon");
  svg.setAttribute("aria-hidden", "true");
  for (const d of ICONS[name]) {
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", d);
    svg.append(path);
  }
  return svg;
}

for (const slot of document.querySelectorAll("[data-icon]")) slot.append(icon(slot.dataset.icon));

function marked(spans) {
  return spans.map((s) => (s.mark ? el("mark", {}, s.text) : document.createTextNode(s.text)));
}

function size(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (bytes >= 1024 && i < units.length - 1) { bytes /= 1024; i++; }
  return `${bytes.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function plural(n, one, many = `${one}s`) {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}

// "Turkmen" for `tk`, named in the reader's own language where the browser can.
const languageName = (() => {
  try {
    const names = new Intl.DisplayNames(navigator.languages, { type: "language" });
    return (code) => names.of(code) ?? code;
  } catch {
    return (code) => code;
  }
})();

const dayFormat = new Intl.DateTimeFormat(navigator.languages, { day: "numeric", month: "short" });
const yearFormat = new Intl.DateTimeFormat(navigator.languages, { day: "numeric", month: "short", year: "numeric" });

function day(secs) {
  if (!secs) return "";
  const date = new Date(secs * 1000);
  const today = new Date();
  if (date.toDateString() === today.toDateString()) return "today";
  return (date.getFullYear() === today.getFullYear() ? dayFormat : yearFormat).format(date);
}

// Clearnet URLs and magnets get a link. Opening an onion or .i2p address in
// an ordinary browser fails at best and leaks a DNS lookup at worst, so there
// those are shown for copying into the right browser; inside the app, omnid's
// own browser opens them over Tor or I2P. Hyphanet pages open through the
// node either way.
function linkable(route) {
  const web = /^https?:\/\//.test(route.url);
  return (web && (route.network === "clearnet" || (EMBEDDED && ["onion", "i2p"].includes(route.network)))) ||
    route.network === "hyphanet" ||
    (route.network === "magnet" && route.url.startsWith("magnet:?xt=urn:btih:"));
}

// Where a link goes. A Hyphanet key opens in omnid's browser as
// http://hyphanet/KEY, which the daemon sends to the node; in any other
// browser, omnid sends it on to the node's own page.
function href(route) {
  if (route.network !== "hyphanet") return route.url;
  const key = route.url.replace(/^(?:freenet|hyphanet):/i, "");
  return EMBEDDED ? `http://hyphanet/${key}` : `/hyphanet/${key}`;
}

// The route a title opens: the one that stays inside an overlay network when
// there is one, then encrypted web, then plain web.
function primaryRoute(hit) {
  const rank = (r) => ({ onion: 0, i2p: 1, hyphanet: 2 }[r.network] ?? (r.url.startsWith("https:") ? 3 : 4));
  return hit.routes.filter((r) => linkable(r) && r.network !== "magnet").sort((a, b) => rank(a) - rank(b))[0];
}

// Onion and .b32.i2p hosts are 56 and 52 characters of noise: keep both
// ends, which is what people compare, and the path. A Hyphanet key keeps its
// type, the start of the key, and the site's own name and path.
function shortAddress(url) {
  const magnet = url.match(/^magnet:\?xt=urn:btih:([0-9a-f]{40})/i);
  if (magnet) return `btih:${magnet[1].slice(0, 12)}…`;
  const key = url.match(/^(?:freenet:|hyphanet:)?([A-Z]{3})@([A-Za-z0-9~-]{8})[^/]*(\/.*?)?\/?$/);
  if (key) return `${key[1]}@${key[2]}…${key[3] ?? ""}`;
  const bare = url.replace(/^[a-z]+:\/\//i, "").replace(/\/$/, "");
  return bare
    .replace(/^((?:[a-z0-9-]+\.)*)([a-z2-7]{8})[a-z2-7]{43}([a-z2-7]{5})\.onion/i, "$1$2…$3.onion")
    .replace(/^([a-z2-7]{8})[a-z2-7]{39,}([a-z2-7]{5})\.b32\.i2p/i, "$1…$2.b32.i2p");
}

// A title that is only an address says nothing a title should.
function looksLikeAddress(text) {
  const t = text.trim();
  return /^https?:\/\/\S+$/i.test(t) || /^(?:freenet:|hyphanet:)?(?:USK|SSK|CHK|KSK)@\S+$/.test(t);
}

// What to call a page with no title: its address, short; for a Hyphanet key,
// the site's own name and path.
function untitled(url) {
  const short = shortAddress(url);
  return short.replace(/^[A-Z]{3}@[^/]*\//, "") || short;
}

// One row per site in Explore: a site's other pages are a "See all" away.
function siteKey(url) {
  const key = url.match(/^(?:freenet:|hyphanet:)?([A-Z]{3}@[^/]*\/[^/]*)/);
  if (key) return key[1];
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return url;
  }
}

// What was typed, read as an address: an onion or .i2p host, a web URL, a
// magnet or a Hyphanet key can be opened or added from the search field as
// well as searched for. A bare clearnet name ("node.js") stays a search.
function typedAddress(text) {
  const t = text.trim();
  if (!t || /\s/.test(t)) return null;
  if (/^magnet:\?\S+$/i.test(t)) return { network: "magnet", url: t };
  const key = t.match(/^(?:freenet:|hyphanet:)?((?:USK|SSK|CHK|KSK)@\S+)$/);
  if (key) return { network: "hyphanet", url: `freenet:${key[1]}` };
  const url = /^https?:\/\//i.test(t) ? t : /^[a-z0-9.-]+\.(onion|i2p)(\/\S*)?$/i.test(t) ? `http://${t}` : null;
  let host;
  try {
    host = url && new URL(url).hostname;
  } catch {
    return null;
  }
  if (!host) return null;
  return { network: host.endsWith(".onion") ? "onion" : host.endsWith(".i2p") ? "i2p" : "clearnet", url };
}

function sealed(route) {
  return ["onion", "i2p", "hyphanet"].includes(route.network) || route.url.startsWith("https:");
}

function badge(route) {
  return el("span", { class: `badge net-${route.network}` },
    route.network === "magnet" ? icon("magnet") : icon(sealed(route) ? "lock" : "unlock"),
    NETWORK_NAMES[route.network] ?? route.network);
}

// The typed address, with what can be done with it: open it (where this
// browser can) and add it to the index.
function addressRow(route) {
  if (!route) return [];
  const open = linkable(route)
    ? el("a", { class: "button small primary", href: href(route), rel: "noreferrer noopener", target: "_blank" }, "Open")
    : null;
  const add = el("button", { class: "button small", type: "button" }, icon("plus"), "Add to index");
  const note = el("p", { class: "submit-result", "aria-live": "polite" });
  add.addEventListener("click", () => submitAddress(route.url, note));
  return [
    el("div", { class: "address-row" },
      badge(route),
      el("span", { class: "addr", title: route.url }, shortAddress(route.url)),
      el("span", { class: "actions" }, open, add)),
    note,
  ];
}

function routeItem(route) {
  const plainWeb = route.network === "clearnet" && route.url.startsWith("http:");
  const text = shortAddress(route.url);
  const address = linkable(route)
    ? el("a", { class: "addr", href: href(route), rel: "noreferrer noopener", target: "_blank", title: route.url }, text)
    : el("span", { class: "addr", title: route.url }, text);
  const copy = el("button", { class: "icon-button", type: "button", "aria-label": "Copy address" }, icon("copy"));
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(route.url);
      copy.replaceChildren(icon("check"));
      copy.classList.add("done");
      copy.setAttribute("aria-label", "Copied");
      setTimeout(() => {
        copy.replaceChildren(icon("copy"));
        copy.classList.remove("done");
        copy.setAttribute("aria-label", "Copy address");
      }, 1400);
    } catch {
      getSelection()?.selectAllChildren(address);
    }
  });
  return el("li", { class: "route" }, badge(route), address, plainWeb ? el("span", { class: "plain" }, "not encrypted") : null, copy);
}

function hitItem(hit) {
  const lang = hit.lang ? { lang: hit.lang } : {};
  const titleText = hit.title_spans.map((s) => s.text).join("");
  const heading = titleText.trim() && !looksLikeAddress(titleText)
    ? marked(hit.title_spans)
    : [untitled(hit.routes[0]?.url || "Untitled")];
  const primary = primaryRoute(hit);
  const title = primary
    ? el("a", { href: href(primary), rel: "noreferrer noopener", target: "_blank" }, ...heading)
    : el("span", {}, ...heading);
  const snippet = hit.snippet.some((s) => s.text) ? el("p", { class: "snippet", ...lang }, ...marked(hit.snippet)) : null;
  const files = hit.files.length
    ? el("details", { class: "files" },
        el("summary", {}, `${plural(hit.files.length, "file")} · ${size(hit.files.reduce((n, f) => n + f.size, 0))}`, icon("chevron")),
        el("ul", {}, ...hit.files.slice(0, 50).map((f) => el("li", {}, `${f.path} · ${size(f.size)}`))))
    : null;
  const seen = hit.first_seen === hit.last_seen || day(hit.first_seen) === day(hit.last_seen)
    ? `seen ${day(hit.last_seen)}`
    : `seen ${day(hit.first_seen)} – ${day(hit.last_seen)}`;
  const meta = [hit.lang && languageName(hit.lang), hit.routes.length > 1 && plural(hit.routes.length, "route"), seen];
  return el("li", { class: "hit", "data-key": hit.content_hash },
    el("h2", lang, title),
    snippet,
    el("ul", { class: "routes" }, ...hit.routes.map(routeItem)),
    files,
    el("p", { class: "meta" }, meta.filter(Boolean).join(" · ")));
}

function skeletons() {
  return [1, 2, 3].map(() => el("li", { class: "skeleton", "aria-hidden": "true" }, el("i"), el("i"), el("i")));
}

function checkedNetworks() {
  return [...document.querySelectorAll("#networks input:checked")].map((i) => i.value);
}

// Networks with the most results first, so the ones that have any are on
// screen; ties keep the usual order.
const NETWORK_ORDER = ["onion", "clearnet", "magnet", "i2p", "hyphanet"];
const byCount = (counts) => (a, b) => (counts[b] ?? 0) - (counts[a] ?? 0) || NETWORK_ORDER.indexOf(a) - NETWORK_ORDER.indexOf(b);

function showCounts(counts) {
  const chips = [...document.querySelectorAll("#networks .chip")];
  for (const chip of chips) {
    const count = chip.querySelector(".count");
    const n = counts[count.dataset.network] ?? 0;
    count.textContent = n.toLocaleString();
    chip.classList.toggle("zero", n === 0);
  }
  const network = (chip) => chip.querySelector("input").value;
  const order = byCount(counts);
  const sorted = [...chips].sort((a, b) => order(network(a), network(b)));
  if (sorted.some((chip, i) => chip !== chips[i])) {
    $("networks").append(...sorted);
    $("networks").scrollLeft = 0;
  }
}

let inflight;
let shown = 0;
let hasMore = false;
let loading = false;
// "See all" lists every page on the chosen networks, with nothing typed.
let browsing = false;

// Results replace each other in place: a row that was already showing stays
// still, and only rows new to the list fade in.
function swapResults(items) {
  const before = new Set([...$("results").querySelectorAll(".hit")].map((li) => li.dataset.key));
  for (const item of items) if (!before.has(item.dataset.key)) item.classList.add("enter");
  $("results").replaceChildren(...items);
}

// The search lives in the address too, so a reload, or the app coming back,
// keeps it.
function rememberQuery(text) {
  const params = new URLSearchParams(location.search);
  if (text) params.set("q", text);
  else params.delete("q");
  const query = params.toString().replace(/=(&|$)/g, "$1");
  history.replaceState(null, "", query ? `?${query}` : location.pathname);
}

async function search({ append = false } = {}) {
  const text = $("q").value.trim();
  $("clear").hidden = !$("q").value && !browsing;
  const networks = checkedNetworks();
  rememberQuery(text);
  $("address").replaceChildren(...addressRow(typedAddress(text)));
  if (!text && !browsing) return explore(networks);
  $("explore").hidden = true;
  if (!networks.length) {
    $("summary").textContent = "Pick at least one network.";
    $("results").replaceChildren();
    hasMore = false;
    $("more").hidden = true;
    return;
  }
  const offset = append ? shown : 0;
  const params = new URLSearchParams({ q: text, limit: String(PAGE), offset: String(offset) });
  if (networks.length < ALL_NETWORKS) params.set("network", networks.join(","));
  if (LANG) params.set("lang", LANG);

  inflight?.abort();
  inflight = new AbortController();
  loading = true;
  // Results swap in place; a skeleton only when the index is slow to answer.
  const slow = append ? null : setTimeout(() => $("results").replaceChildren(...skeletons()), 150);
  try {
    const response = await fetch(`/api/search?${params}`, { signal: inflight.signal });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || response.statusText);
    clearTimeout(slow);

    const items = body.hits.map(hitItem);
    if (append) {
      for (const item of items) item.classList.add("enter");
      $("results").append(...items);
    } else {
      swapResults(items);
      // A typed address with nothing indexed yet: its row above says what to do.
      if (!items.length && !typedAddress(text)) $("results").append(noResults(text, body.network_counts, networks));
    }
    shown = offset + body.hits.length;
    showCounts(body.network_counts);
    const relaxed = body.fuzzy
      ? body.relaxed ? " · nothing has every word, even spelt a little differently, so these have some" : " · nothing matches as typed; these are spelt a little differently"
      : body.relaxed ? " · no page has every word, so these have some" : "";
    if (browsing && !text) {
      const back = el("button", { type: "button" }, "Back to Explore");
      back.addEventListener("click", () => {
        browsing = false;
        for (const input of document.querySelectorAll("#networks input")) input.checked = true;
        search();
      });
      $("summary").replaceChildren(`${plural(body.total, "page")} on ${networks.map((n) => NETWORK_NAMES[n] ?? n).join(", ")}`, back);
    } else {
      $("summary").textContent = body.total ? `${plural(body.total, "result")} · ${body.took_ms} ms${relaxed}` : "";
    }
    // Mirrors fold together, so `total` counts addresses, not results; a
    // short page means the end.
    hasMore = body.hits.length === PAGE;
    $("more").hidden = !hasMore || "IntersectionObserver" in window;
  } catch (error) {
    clearTimeout(slow);
    if (error.name !== "AbortError") {
      $("summary").textContent = "";
      $("results").replaceChildren(el("li", { class: "empty" },
        el("h2", {}, "Search did not go through"),
        el("p", {}, `OMNId did not answer (${error.message}). It may be restarting; try again in a moment.`)));
    }
  } finally {
    loading = false;
  }
}

function noResults(text, counts, networks) {
  const elsewhere = Object.entries(counts).filter(([network, n]) => n > 0 && !networks.includes(network));
  const tips = el("p", {}, "Try fewer or different words, or a site:host filter.");
  const item = el("li", { class: "empty" }, el("h2", {}, `Nothing matches “${text}”`), tips);
  for (const [network, n] of elsewhere) {
    const button = el("button", { class: "button small", type: "button" },
      `Show ${plural(n, "result")} in ${NETWORK_NAMES[network] ?? network}`);
    button.addEventListener("click", () => {
      document.querySelector(`#networks input[value="${network}"]`).checked = true;
      search();
    });
    item.append(button);
  }
  return item;
}

// With nothing typed: the best of each network in the index, so there is
// always somewhere to go.
async function explore(networks) {
  inflight?.abort();
  inflight = new AbortController();
  $("results").replaceChildren();
  $("summary").textContent = "";
  hasMore = false;
  $("more").hidden = true;
  try {
    const all = await (await fetch("/api/search?q=&limit=1", { signal: inflight.signal })).json();
    showCounts(all.network_counts);
    const present = [...NETWORK_ORDER].sort(byCount(all.network_counts))
      .filter((n) => networks.includes(n) && (all.network_counts[n] ?? 0) > 0);
    const groups = await Promise.all(present.map(async (network) => {
      // Enough pages to find five sites among them.
      const params = new URLSearchParams({ q: "", limit: "20", network });
      if (LANG) params.set("lang", LANG);
      const body = await (await fetch(`/api/search?${params}`, { signal: inflight.signal })).json();
      return exploreGroup(network, all.network_counts[network], body.hits);
    }));
    const indexed = Object.values(all.network_counts).reduce((a, b) => a + b, 0);
    $("explore-lists").replaceChildren(...(groups.length ? groups : [indexed ? filteredOut() : emptyIndex()]));
    $("explore").hidden = false;
  } catch (error) {
    if (error.name !== "AbortError") $("summary").textContent = `OMNId did not answer (${error.message}).`;
  }
}

const GROUP_TITLES = { onion: "Onion sites", clearnet: "On the web", magnet: "Torrents", i2p: "I2P sites", hyphanet: "Hyphanet sites" };

function exploreGroup(network, count, hits) {
  const all = el("button", { type: "button" }, "See all");
  all.addEventListener("click", () => {
    browsing = true;
    for (const input of document.querySelectorAll("#networks input")) input.checked = input.value === network;
    search();
    scrollTo({ top: 0 });
  });
  const seen = new Set();
  const sites = hits.flatMap((hit) => {
    // The group's own network first: a mirror listed under the web shows its web address.
    const primary = hit.routes.find((r) => r.network === network) ?? primaryRoute(hit) ?? hit.routes[0];
    const site = siteKey(primary.url);
    if (seen.has(site)) return [];
    seen.add(site);
    return [[hit, primary]];
  }).slice(0, 5).map(([hit, primary]) => {
    const name = hit.title && !looksLikeAddress(hit.title) ? hit.title : untitled(primary.url);
    const address = shortAddress(primary.url);
    // An untitled site is named by its address; once is enough.
    const body = [el("span", { class: "dot" }), el("b", {}, name), name === address ? null : el("small", {}, address)];
    const row = linkable(primary)
      ? el("a", { href: href(primary), rel: "noreferrer noopener", target: "_blank" }, ...body)
      : el("div", { title: primary.url }, ...body);
    return el("li", { class: `site net-${network}` }, row);
  });
  return el("div", { class: "explore-group" },
    el("div", { class: "explore-head" }, el("h2", {}, GROUP_TITLES[network] ?? network), el("span", { class: "count" }, count.toLocaleString()), all),
    el("ul", { class: "site-list" }, ...sites));
}

// Pages exist, but not on the networks the chips leave on.
function filteredOut() {
  const all = el("button", { class: "button small", type: "button" }, "Search every network");
  all.addEventListener("click", () => {
    for (const input of document.querySelectorAll("#networks input")) input.checked = true;
    search();
  });
  return el("div", { class: "empty" },
    el("h2", {}, "Nothing on the networks you picked"),
    el("p", {}, "Your index has pages on other networks. Turn their chips back on to see them."),
    all);
}

function emptyIndex() {
  return el("div", { class: "empty" },
    el("h2", {}, "Your index is empty so far"),
    el("p", {}, "OMNId fills it by crawling: it starts from a few well-known onion and I2P sites and follows their links. Add an address below to start somewhere else."));
}

// The trust strip: each network's state in plain words.
const ROUTERS = [
  { network: "onion", name: "Tor" },
  { network: "magnet", name: "Torrents" },
  { network: "i2p", name: "I2P" },
  { network: "hyphanet", name: "Hyphanet" },
  { network: "clearnet", name: "Web", panelOnly: true },
];

const STATE_WORDS = { ready: "Ready", starting: "Connecting", unreachable: "Not running", disabled: "Off", unsupported: "Not available" };

// Inside the Android app, whether the i2pd app is installed: the app says so when the page loads
// and whenever it comes back to the front. null elsewhere.
let i2pdInstalled = null;
window.omnidApp = {
  i2pd(installed) {
    i2pdInstalled = Boolean(installed);
    refreshStatus();
  },
};

// Where a router's detail says it listens, in words: on this device, or the
// other machine's address. With several routers ("…; no … at …"), the first
// that answers.
function whereIs(detail) {
  const routers = detail.split("; ");
  const answering = routers.find((r) => !r.startsWith("no ")) ?? routers[0];
  const url = answering.match(/https?:\/\/[^\s/]+/)?.[0];
  if (!url) return "";
  const host = new URL(url).hostname;
  return ["127.0.0.1", "localhost", "[::1]"].includes(host) ? "on this device" : `at ${host}`;
}

function explain(network, router) {
  const state = router?.state ?? "disabled";
  const detail = router?.detail ?? "";
  switch (network) {
    case "onion":
      if (state === "ready") return "Onion sites, and every page you open in OMNId's browser, go through Tor, built into OMNId.";
      if (state === "starting") return `Connecting to Tor (${detail}). Onion sites open once it is ready.`;
      return "Tor is off, so onion sites cannot be reached.";
    case "magnet":
      if (state === "ready") return "Finds torrent names and file lists on the BitTorrent network. Files themselves are never downloaded.";
      if (state === "starting") return "Joining the BitTorrent network to look up torrents.";
      return "Torrent lookups are off.";
    case "i2p":
      if (state === "ready") return `I2P sites go through the I2P router ${whereIs(detail) || "on this device"}.`;
      if (i2pdInstalled === true) return "The i2pd app is installed, but its router is not running. Start it there and OMNId uses it right away, or use a router on your own computer: ⋮ → Settings → I2P router.";
      if (i2pdInstalled === false) return "Reaching .i2p sites needs an I2P router: the free i2pd app on this phone, from F-Droid, which OMNId uses as soon as it runs, or one on your own computer, over Tailscale for instance: ⋮ → Settings → I2P router.";
      return "Reaching .i2p sites needs an I2P router, such as the i2pd app, running on this device.";
    case "hyphanet":
      if (state === "ready") return `Hyphanet sites come through the Hyphanet node ${whereIs(detail) || "on this device"}.`;
      if (EMBEDDED) return `No Hyphanet node answers ${whereIs(detail) || "on this device"}. Use one on your own computer, over Tailscale for instance: ⋮ → Settings → Hyphanet node.`;
      return "Hyphanet sites need a Hyphanet node running on this device.";
    case "clearnet":
      return EMBEDDED
        ? "OMNId's crawler reads web pages directly from this device. Pages you open in its browser go through Tor."
        : "OMNId's crawler reads web pages directly from this device.";
  }
  return detail;
}

// The I2P row's way to the i2pd app, inside the Android app, which opens it or its F-Droid page.
function routerAction(network, state) {
  if (network !== "i2p" || state === "ready" || i2pdInstalled === null) return null;
  return el("a", { class: "button small", href: "omnid:i2pd" }, i2pdInstalled ? "Open i2pd" : "Get i2pd");
}

let statusTimer;

async function refreshStatus() {
  try {
    const status = await (await fetch("/api/status")).json();
    const byNetwork = Object.fromEntries(status.routers.map((r) => [r.network, r]));
    const strip = ROUTERS.filter((r) => !r.panelOnly);
    $("trust-routers").replaceChildren(...strip.map(({ network, name }) =>
      el("span", { class: `trust-router ${byNetwork[network]?.state ?? "disabled"}` }, el("span", { class: "state-dot" }), name)));
    // The dots carry state by colour alone; a screen reader hears it in words.
    const spoken = strip.map(({ network, name }) => `${name}: ${STATE_WORDS[byNetwork[network]?.state ?? "disabled"]}`);
    const c = status.crawl;
    // Crawling while there is anything to fetch, not only at the instant a page is loading.
    const crawlWord = c.paused ? "Paused" : c.inflight || c.queued ? "Crawling" : "Idle";
    $("trust").setAttribute("aria-label", `${spoken.join(", ")}. Crawler: ${crawlWord}. Show details`);
    $("router-list").replaceChildren(...ROUTERS.map(({ network, name }) => {
      const state = byNetwork[network]?.state ?? "disabled";
      return el("li", { class: state },
        el("span", { class: "state-dot" }),
        el("b", {}, name, el("span", {}, STATE_WORDS[state] ?? state)),
        el("p", {}, explain(network, byNetwork[network])),
        routerAction(network, state));
    }));

    const reason = status.pause_reason ? `: ${status.pause_reason}` : "";
    $("trust-crawl").textContent = crawlWord;
    $("crawl-detail").textContent =
      `${c.paused ? `Crawling is paused${reason}.` : crawlWord === "Crawling" ? "Crawling now." : "Waiting for new addresses to crawl."} ` +
      `${plural(status.index.documents, "page")} indexed, ${plural(c.known, "address", "addresses")} found.`;
    const pause = $("pause");
    pause.hidden = false;
    pause.replaceChildren(icon(c.paused ? "play" : "pause"), c.paused ? "Resume" : "Pause");
    pause.dataset.paused = String(c.paused);
  } catch {
    $("trust-routers").replaceChildren(el("span", { class: "trust-router unreachable" }, el("span", { class: "state-dot" }), "OMNId is not answering"));
    $("trust-crawl").textContent = "";
  }
}

function startStatus() {
  clearInterval(statusTimer);
  refreshStatus();
  statusTimer = setInterval(refreshStatus, 5000);
}

document.addEventListener("visibilitychange", () => (document.hidden ? clearInterval(statusTimer) : startStatus()));

$("trust").addEventListener("click", () => {
  const open = $("trust").getAttribute("aria-expanded") !== "true";
  $("trust").setAttribute("aria-expanded", String(open));
  $("trust-panel").hidden = !open;
});

let debounce;
$("q").addEventListener("input", () => {
  clearTimeout(debounce);
  debounce = setTimeout(search, 120);
});
$("clear").addEventListener("click", () => {
  $("q").value = "";
  if (browsing) {
    browsing = false;
    for (const input of document.querySelectorAll("#networks input")) input.checked = true;
  }
  search();
  $("q").focus();
});
$("networks").addEventListener("change", () => search());
$("search").addEventListener("submit", (event) => {
  event.preventDefault();
  clearTimeout(debounce);
  search();
  $("q").blur();
});
$("more").addEventListener("click", () => search({ append: true }));

// The next page loads before the reader reaches the end of this one.
if ("IntersectionObserver" in window) {
  new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting) && hasMore && !loading) search({ append: true });
  }, { rootMargin: "800px 0px" }).observe($("sentinel"));
}

$("pause").addEventListener("click", async () => {
  const paused = $("pause").dataset.paused !== "true";
  await fetch("/api/crawl", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ paused }),
  });
  refreshStatus();
});

// Queues an address to crawl next and says how that went in `result`.
async function submitAddress(url, result) {
  try {
    const response = await fetch("/api/submit", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ url }),
    });
    const body = await response.json();
    result.className = `submit-result ${response.ok ? "ok" : "error"}`;
    result.textContent = response.ok
      ? `Added. OMNId will crawl ${body.accepted} (${NETWORK_NAMES[body.network] ?? body.network}) next.`
      : body.error;
    return response.ok;
  } catch (error) {
    result.className = "submit-result error";
    result.textContent = `OMNId did not answer (${error.message}).`;
    return false;
  }
}

$("submit").addEventListener("submit", async (event) => {
  event.preventDefault();
  const url = $("url").value.trim();
  if (url && (await submitAddress(url, $("submit-result")))) $("url").value = "";
});

// /?q=words opens with that search, so a search can be linked to;
// &networks=onion,i2p with those networks' chips on (the app's Settings).
$("q").value = new URLSearchParams(location.search).get("q") ?? "";
const startNetworks = new URLSearchParams(location.search).get("networks");
if (startNetworks) {
  const chosen = new Set(startNetworks.split(","));
  for (const input of document.querySelectorAll("#networks input")) input.checked = chosen.has(input.value);
}
startStatus();
search();

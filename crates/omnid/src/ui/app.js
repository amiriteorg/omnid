"use strict";

// Everything shown here came from crawled pages. It is only ever inserted as
// text (append/textContent), never as markup; matches are marked by wrapping
// text nodes in <mark>.

const $ = (id) => document.getElementById(id);
const ALL_NETWORKS = 5;
const PAGE = 20;

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.filter((c) => c !== null));
  return node;
}

function marked(spans) {
  return spans.map((s) => (s.mark ? el("mark", {}, s.text) : document.createTextNode(s.text)));
}

function date(secs) {
  return secs ? new Date(secs * 1000).toISOString().slice(0, 10) : "?";
}

function size(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (bytes >= 1024 && i < units.length - 1) { bytes /= 1024; i++; }
  return `${bytes.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// Only clearnet URLs and magnets get a link. Opening an onion or .i2p
// address in an ordinary browser fails at best and leaks a DNS lookup at
// worst, so those are shown for copying into the right browser.
function routeItem(route) {
  const linkable =
    (route.network === "clearnet" && /^https?:\/\//.test(route.url)) ||
    (route.network === "magnet" && route.url.startsWith("magnet:?xt=urn:btih:"));
  const target = linkable
    ? el("a", { href: route.url, rel: "noreferrer noopener", target: "_blank" }, route.url)
    : el("code", {}, route.url);
  const copy = el("button", { type: "button", title: "Copy address" }, "copy");
  copy.addEventListener("click", async () => {
    await navigator.clipboard?.writeText(route.url);
    copy.textContent = "copied";
    setTimeout(() => (copy.textContent = "copy"), 1200);
  });
  return el("li", {}, el("span", { class: `net net-${route.network}` }, route.network), target, copy);
}

function hitItem(hit) {
  const files = hit.files.length
    ? el("details", {},
        el("summary", {}, `${plural(hit.files.length, "file")}, ${size(hit.files.reduce((n, f) => n + f.size, 0))}`),
        el("ul", {}, ...hit.files.map((f) => el("li", {}, `${f.path} (${size(f.size)})`))))
    : null;
  const title = hit.title_spans.length ? marked(hit.title_spans) : [hit.routes[0]?.url || "(untitled)"];
  const snippet = hit.snippet.some((s) => s.text) ? el("p", { class: "desc" }, ...marked(hit.snippet)) : null;
  return el("li", { class: "hit" },
    el("h2", {}, ...title),
    snippet,
    el("ul", { class: "routes" }, ...hit.routes.map(routeItem)),
    files,
    el("p", { class: "meta" },
      `${plural(hit.routes.length, "route")} · first seen ${date(hit.first_seen)} · last seen ${date(hit.last_seen)}`));
}

let inflight;
let shown = 0;
let total = 0;

async function search({ append = false } = {}) {
  const checked = [...document.querySelectorAll("#networks input:checked")].map((i) => i.value);
  if (!checked.length) {
    $("summary").textContent = "Pick at least one network.";
    $("results").replaceChildren();
    $("more").hidden = true;
    return;
  }
  const offset = append ? shown : 0;
  const params = new URLSearchParams({ q: $("q").value.trim(), limit: String(PAGE), offset: String(offset) });
  if (checked.length < ALL_NETWORKS) params.set("network", checked.join(","));

  inflight?.abort();
  inflight = new AbortController();
  try {
    const response = await fetch(`/api/search?${params}`, { signal: inflight.signal });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || response.statusText);

    const items = body.hits.map(hitItem);
    if (append) $("results").append(...items);
    else $("results").replaceChildren(...items);
    shown = offset + body.hits.length;
    total = body.total;

    for (const count of document.querySelectorAll(".count")) {
      const n = body.network_counts[count.dataset.network] ?? 0;
      count.textContent = n ? `(${n})` : "";
    }
    const relaxed = body.relaxed ? " No page has every word; showing pages with any of them." : "";
    $("summary").textContent = `${plural(total, "page")} in ${body.took_ms} ms.${relaxed}`;
    // Mirrors fold together, so `total` counts addresses, not results; a
    // short page means the end.
    $("more").hidden = body.hits.length < PAGE;
  } catch (error) {
    if (error.name !== "AbortError") $("summary").textContent = `Search failed: ${error.message}`;
  }
}

async function refreshStatus() {
  try {
    const status = await (await fetch("/api/status")).json();
    $("routers").replaceChildren(...status.routers.map((r) =>
      el("li", { class: `router ${r.state}`, title: `${r.state}: ${r.detail}` }, el("span", { class: "dot" }), r.network)));
    const c = status.crawl;
    const state = c.paused ? `paused${status.pause_reason ? ` (${status.pause_reason})` : ""}` : "crawling";
    $("crawl").textContent =
      `${status.index.documents} documents · ${c.known} known · ${state} · fetched ${c.fetched} · ` +
      `indexed ${c.indexed} · in flight ${c.inflight} · failed ${c.failed} · blocked ${c.blocked}`;
    const pause = $("pause");
    pause.hidden = false;
    pause.textContent = c.paused ? "Resume" : "Pause";
    pause.dataset.paused = String(c.paused);
  } catch {
    $("routers").replaceChildren(el("li", { class: "router" }, el("span", { class: "dot" }), "daemon unreachable"));
  }
}

let debounce;
$("q").addEventListener("input", () => {
  clearTimeout(debounce);
  debounce = setTimeout(search, 200);
});
$("networks").addEventListener("change", () => search());
$("search").addEventListener("submit", (event) => {
  event.preventDefault();
  search();
});
$("more").addEventListener("click", () => search({ append: true }));

$("pause").addEventListener("click", async () => {
  const paused = $("pause").dataset.paused !== "true";
  await fetch("/api/crawl", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ paused }),
  });
  refreshStatus();
});

$("submit").addEventListener("submit", async (event) => {
  event.preventDefault();
  const url = $("url").value.trim();
  if (!url) return;
  try {
    const response = await fetch("/api/submit", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ url }),
    });
    const body = await response.json();
    $("submit-result").textContent = response.ok ? `Queued ${body.accepted} (${body.network})` : body.error;
    if (response.ok) $("url").value = "";
  } catch (error) {
    $("submit-result").textContent = `Could not reach the daemon: ${error.message}`;
  }
});

refreshStatus();
setInterval(refreshStatus, 5000);
search();

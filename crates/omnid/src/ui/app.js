"use strict";

// Everything shown here came from crawled pages. It is only ever inserted as
// text (append/textContent), never as markup.

const $ = (id) => document.getElementById(id);
const ALL_NETWORKS = 5;

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.filter((c) => c !== null));
  return node;
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
  const routes = hit.routes.length;
  const files = hit.files.length
    ? el("details", {},
        el("summary", {}, `${hit.files.length} file${hit.files.length === 1 ? "" : "s"}`),
        el("ul", {}, ...hit.files.map((f) => el("li", {}, `${f.path} (${size(f.size)})`))))
    : null;
  return el("li", { class: "hit" },
    el("h2", {}, hit.title || hit.routes[0]?.url || "(untitled)"),
    hit.description ? el("p", { class: "desc" }, hit.description) : null,
    el("ul", { class: "routes" }, ...hit.routes.map(routeItem)),
    files,
    el("p", { class: "meta" },
      `${routes} route${routes === 1 ? "" : "s"} · first seen ${date(hit.first_seen)} · last seen ${date(hit.last_seen)}`));
}

let inflight;
async function search() {
  const checked = [...document.querySelectorAll("#networks input:checked")].map((i) => i.value);
  if (!checked.length) {
    $("summary").textContent = "Pick at least one network.";
    $("results").replaceChildren();
    return;
  }
  const params = new URLSearchParams({ q: $("q").value.trim(), limit: "30" });
  if (checked.length < ALL_NETWORKS) params.set("network", checked.join(","));

  inflight?.abort();
  inflight = new AbortController();
  try {
    const response = await fetch(`/api/search?${params}`, { signal: inflight.signal });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || response.statusText);
    const n = body.hits.length;
    $("summary").textContent = `${n} result${n === 1 ? "" : "s"} in ${body.took_ms} ms`;
    $("results").replaceChildren(...body.hits.map(hitItem));
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
    $("crawl").textContent =
      `${status.index.documents} documents · fetched ${c.fetched} · indexed ${c.indexed} · ` +
      `queued ${c.queued} · in flight ${c.inflight} · failed ${c.failed} · blocked ${c.blocked}`;
  } catch {
    $("routers").replaceChildren(el("li", { class: "router" }, el("span", { class: "dot" }), "daemon unreachable"));
  }
}

let debounce;
$("q").addEventListener("input", () => {
  clearTimeout(debounce);
  debounce = setTimeout(search, 200);
});
$("networks").addEventListener("change", search);
$("search").addEventListener("submit", (event) => {
  event.preventDefault();
  search();
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

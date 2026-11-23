// Sends every request through omnid's SOCKS5 proxy with the site the browser
// is on as the username: omnid gives each site Tor circuits of its own, as Tor
// Browser does. The port and password come from the app. A request this does
// not route goes to Gecko's own proxy setting, a port nothing listens on, and
// fails. Never return null from the proxy listener: that sends a request
// direct.
"use strict";

let proxy = null;

// { port, password }, asked of the app once and again after a proxy error.
function config() {
  proxy ??= browser.runtime
    .sendNativeMessage("omnid", "proxy")
    .then((text) => {
      const answer = JSON.parse(text);
      if (!Number.isInteger(answer?.port) || answer.port <= 0 || typeof answer.password !== "string") {
        throw new Error("omnid is not running");
      }
      return answer;
    })
    .catch((error) => {
      proxy = null;
      throw error;
    });
  return proxy;
}

// A URL's host as omnid routes it: lower case, without trailing dots (which
// name the same host), and for a blob: URL the host of the page that made
// it. "" when it has none (about:blank, about:srcdoc, data:).
function hostOf(url) {
  if (!url) return "";
  try {
    const parsed = new URL(url);
    if (parsed.protocol === "blob:") return hostOf(url.slice("blob:".length));
    return parsed.hostname.toLowerCase().replace(/\.+$/, "");
  } catch {
    return "";
  }
}

// The site of each tab's top-level page, for requests that do not say which
// page they belong to (an about:blank window a page writes into).
const tabSites = new Map();

// The host of the top-level page a request belongs to. A request that cannot
// say gets a username of its own, used once: omnid puts it on circuits no
// other request shares, rather than one circuit for every site.
function site(details) {
  const candidates =
    details.type === "main_frame"
      ? [details.url]
      : [
          details.frameAncestors?.length ? details.frameAncestors[details.frameAncestors.length - 1].url : null,
          details.documentUrl,
          details.originUrl,
        ];
  for (const url of candidates) {
    const host = hostOf(url);
    if (host) return host;
  }
  const host = tabSites.get(details.tabId);
  if (host) return host;
  return "~" + crypto.randomUUID();
}

// Which overlay network a host is on, if one the browser reaches through a
// router or node of the owner's own.
function overlay(host) {
  if (host.endsWith(".i2p")) return "i2p";
  if (host === "hyphanet") return "hyphanet";
  return null;
}

// The network a host is on, for telling where a request crosses between them.
function network(host) {
  if (host.endsWith(".onion")) return "onion";
  return overlay(host) ?? "clearnet";
}

// A page loads nothing from I2P or Hyphanet unless it is there itself:
// otherwise any site could learn, by loading an image from one, that this
// phone runs omnid with a router or node. Going there from the top (a typed
// address or a tapped link) is checked by the app, which refuses scripts and
// redirects.
function crossesIntoOverlay(details) {
  if (details.type === "main_frame") return false;
  const target = overlay(hostOf(details.url));
  return target !== null && overlay(site(details)) !== target;
}

// The eepsites tabs are loading at the top, by request.
const loading = new Map();

// Gecko's own question, 3 s into loading an address typed as http:// and
// upgraded to HTTPS: does the site answer over HTTP, at http://<host>/? If it
// does, Gecko stops waiting for HTTPS, and the app's error page reloads the
// address over HTTP. The question comes from no page and no tab, so it would
// go to the dead-end port, and an eepsite without HTTPS kept the page waiting
// for its HTTPS attempt to fail, about 25 s. It goes through as the site, on
// that site's own I2P identity, and only while a tab is loading that eepsite:
// the router reaches no site the address did not already take it to.
// Hyphanet needs none of this: omnid refuses HTTPS to the node at once.
function httpsOnlyQuestion(details) {
  if (details.type === "main_frame" || details.tabId !== -1 || details.frameAncestors?.length) return null;
  if (hostOf(details.documentUrl) || hostOf(details.originUrl)) return null;
  let url;
  try {
    url = new URL(details.url);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" || url.port || url.username || url.pathname !== "/" || url.search) return null;
  const host = hostOf(details.url);
  if (!host.endsWith(".i2p")) return null;
  for (const loaded of loading.values()) if (loaded === host) return host;
  return null;
}

const topLevel = { urls: ["<all_urls>"], types: ["main_frame"] };
browser.webRequest.onBeforeRequest.addListener((details) => {
  const host = hostOf(details.url);
  if (host.endsWith(".i2p")) loading.set(details.requestId, host);
  else loading.delete(details.requestId);
}, topLevel);
const loaded = (details) => loading.delete(details.requestId);
browser.webRequest.onCompleted.addListener(loaded, topLevel);
browser.webRequest.onErrorOccurred.addListener(loaded, topLevel);

browser.proxy.onRequest.addListener(
  async (details) => {
    const { port, password } = await config();
    const asked = httpsOnlyQuestion(details);
    if (asked) return { type: "socks", host: "127.0.0.1", port, username: asked, password, proxyDNS: true };
    // To Gecko's dead-end port, where it fails like any request this does not route.
    if (crossesIntoOverlay(details)) return { type: "socks", host: "127.0.0.1", port: 9, proxyDNS: true };
    return { type: "socks", host: "127.0.0.1", port, username: site(details), password, proxyDNS: true };
  },
  { urls: ["<all_urls>"] },
);

// omnid may have restarted.
browser.proxy.onError.addListener(() => {
  proxy = null;
});

// What every page asks in, as Tor Browser sends it: GeckoView would add the
// phone's own languages.
const ACCEPT_LANGUAGE = "en-US,en;q=0.5";

// What I2P's HTTP proxies, i2pd's and Java I2P's, send as every client's
// User-Agent. Eepsites expect it, and some refuse any other as an inproxy's:
// stats.i2p answers "403 Inproxy access denied". The browser's streams to
// eepsites (SAM, or a CONNECT through i2pd) pass on its own.
const I2P_USER_AGENT = "MYOB/6.66 (AN/ON)";

browser.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    if (details.type === "main_frame") tabSites.set(details.tabId, hostOf(details.url));
    const to = network(hostOf(details.url));
    const from = network(site(details));
    const crossing = to !== from;
    const headers = [];
    for (const header of details.requestHeaders) {
      const name = header.name.toLowerCase();
      if (name === "accept-language") continue;
      if (to === "i2p" && name === "user-agent") {
        headers.push({ name: header.name, value: I2P_USER_AGENT });
        continue;
      }
      // Leaving the network the page is on, its address stays behind: even an
      // origin says "an onion site" or "an eepsite" sent you.
      if (crossing && name === "referer") continue;
      if (crossing && name === "origin") {
        headers.push({ name: header.name, value: "null" });
        continue;
      }
      headers.push(header);
    }
    headers.push({ name: "Accept-Language", value: ACCEPT_LANGUAGE });
    return { requestHeaders: headers };
  },
  { urls: ["<all_urls>"] },
  ["blocking", "requestHeaders"],
);

browser.tabs?.onRemoved?.addListener((tabId) => tabSites.delete(tabId));

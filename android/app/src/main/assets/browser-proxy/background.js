// Sends every request through omnid's SOCKS5 proxy with the site the browser
// is on as the username: omnid gives each site Tor circuits of its own, as Tor
// Browser does. The port and password come from the app. A request this does
// not route goes to Gecko's own proxy setting, a port nothing listens on, and
// fails.
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

// The host of the top-level page a request belongs to.
function site(details) {
  const top =
    details.type === "main_frame"
      ? details.url
      : details.frameAncestors?.length
        ? details.frameAncestors[details.frameAncestors.length - 1].url
        : (details.documentUrl ?? details.originUrl);
  try {
    return new URL(top).hostname || "-";
  } catch {
    return "-";
  }
}

// Which overlay network a host is on, if one the browser reaches through a
// router or node of the owner's own.
function overlay(host) {
  if (host.endsWith(".i2p")) return "i2p";
  if (host === "hyphanet") return "hyphanet";
  return null;
}

// A page loads nothing from I2P or Hyphanet unless it is there itself:
// otherwise any site could learn, by loading an image from one, that this
// phone runs omnid with a router or node. Following a link there is the
// person's own choice and is not affected.
function crossesIntoOverlay(details) {
  if (details.type === "main_frame") return false;
  let target;
  try {
    target = overlay(new URL(details.url).hostname);
  } catch {
    return false;
  }
  return target !== null && overlay(site(details)) !== target;
}

browser.proxy.onRequest.addListener(
  async (details) => {
    const { port, password } = await config();
    // To Gecko's dead-end port, where it fails like any request this does not route.
    if (crossesIntoOverlay(details)) return { type: "socks", host: "127.0.0.1", port: 9, proxyDNS: true };
    return { type: "socks", host: "127.0.0.1", port, username: site(details), password, proxyDNS: true };
  },
  { urls: ["<all_urls>"] },
);

// omnid may have restarted on another port.
browser.proxy.onError.addListener(() => {
  proxy = null;
});

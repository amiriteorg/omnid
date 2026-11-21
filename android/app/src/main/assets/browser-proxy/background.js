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

browser.proxy.onRequest.addListener(
  async (details) => {
    const { port, password } = await config();
    return { type: "socks", host: "127.0.0.1", port, username: site(details), password, proxyDNS: true };
  },
  { urls: ["<all_urls>"] },
);

// omnid may have restarted on another port.
browser.proxy.onError.addListener(() => {
  proxy = null;
});

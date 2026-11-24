//! The multi-network fetch layer. [`Gateway::fetch`] takes any [`Source`]
//! and routes it to the transport that can reach it:
//!
//! | Network  | Transport                                           |
//! |----------|-----------------------------------------------------|
//! | clearnet | reqwest, with a resolver that refuses private IPs   |
//! | onion    | arti, in-process, HTTP/1.1 over a Tor stream        |
//! | i2p      | i2pd's HTTP proxy, of one router or several; SAM    |
//! |          | for the browser, an I2P identity per site           |
//! | hyphanet | a node's FProxy, of one node or several             |
//! | ipfs     | an IPFS gateway's paths, of one gateway or several  |
//! | nostr    | relays over WebSocket, [`Gateway::nostr`]            |
//! | gemini   | TLS to the capsule, on the network its host is on   |
//! | gopher   | TCP to the hole, on the network its host is on      |
//! | magnet   | not yet: needs the DHT module                       |
//!
//! Redirects are resolved here, against the source that issued them, so a
//! clearnet page that redirects to an onion is followed over Tor.
//!
//! [`Gateway::download`] fetches one file a user asked for, in byte ranges
//! over several connections at once.

mod download;
mod gemini;
mod gopher;
mod guard;
mod http;
mod nostr;
mod routers;
mod sam;
mod small_web;
mod tailnet;
mod tor;
mod ws;

/// The parsers' entry points for the fuzz targets in `fuzz/`, built only by
/// cargo-fuzz (`cfg(fuzzing)`). Not part of the API.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzzing {
    pub use crate::download::fuzz::headers as download_headers;
    pub use crate::gemini::fuzz::header as gemini_header;
    pub use crate::http::fuzz::{decode as http_decode, response as http_response};
    pub use crate::nostr::fuzz::relay as nostr_relay;
    pub use crate::sam::fuzz::replies as sam_replies;
    pub use crate::ws::fuzz::frames as ws_frames;
}

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use omnid_core::{Network, Source};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

use crate::routers::Routers;
use crate::tor::Circuits;

pub use download::{
    Download, DownloadError, DownloadOptions, Expected, MAX_CONNECTIONS, Progress, Route, sanitize_filename,
};
pub use gopher::{MENU_TYPE as GOPHER_MENU_TYPE, content_type as gopher_content_type};
pub use guard::is_public_ip;
pub use nostr::{Event as NostrEvent, Filter as NostrFilter, MAX_EVENTS as NOSTR_MAX_EVENTS};
pub use sam::{DEFAULT_PORT as DEFAULT_SAM_PORT, beside as sam_beside};
pub use small_web::SmallWeb;

/// The name `http://hyphanet/USK@…` gives the Hyphanet node's FProxy, for the
/// app's browser and the SOCKS proxy. A single label, so no real host has it;
/// it leads to the configured node and nowhere else.
pub const HYPHANET_HOST: &str = "hyphanet";

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Where i2pd's HTTP proxy, a Hyphanet node's FProxy and an IPFS node's
/// gateway (kubo's) listen by default.
pub const DEFAULT_I2P_PROXY: &str = "http://127.0.0.1:4444";
pub const DEFAULT_FPROXY: &str = "http://127.0.0.1:8888";
pub const DEFAULT_IPFS_GATEWAY: &str = "http://127.0.0.1:8080";
/// Relays that let anyone read, asked for Nostr events by default.
pub const DEFAULT_NOSTR_RELAYS: [&str; 4] =
    ["wss://nos.lol", "wss://relay.primal.net", "wss://nostr.mom", "wss://relay.damus.io"];

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub user_agent: String,
    pub clearnet_timeout: Duration,
    /// Overlay networks build circuits or tunnels per request and are slow.
    pub overlay_timeout: Duration,
    /// Where arti keeps its state and directory cache. `None` disables Tor.
    pub tor_dir: Option<PathBuf>,
    /// i2pd HTTP proxies, best first: routers on this machine or elsewhere
    /// (the owner's computer over Tailscale, say). Each reaches all of I2P;
    /// one that cannot be reached is passed over until it is back. Empty
    /// disables I2P.
    pub i2p_proxies: Vec<Url>,
    /// I2P routers' SAM bridges, `sam://host:port`, best first. Through one,
    /// each site the browser is on and each user of the proxy for other apps
    /// gets an I2P identity of its own, on 3-hop tunnels; without one that
    /// answers, they share the HTTP proxy's one destination. Empty by
    /// default; [`sam_beside`] gives routers' usual ones.
    pub i2p_sam: Vec<Url>,
    /// Hyphanet nodes' FProxy, best first, in the same way. Empty disables
    /// Hyphanet.
    pub fproxies: Vec<Url>,
    /// IPFS gateways, best first, in the same way: path gateways that serve
    /// `/ipfs/<CID>/…` and `/ipns/<name>/…`, such as an IPFS node's own. A
    /// page without a gateway's headers (`Ipfs-Uri`, `X-Ipfs-Roots` or
    /// `X-Ipfs-Path`) is not from one, and is not taken. IPFS
    /// is no anonymity network: the node asks its peers for the content
    /// from its own address. Empty disables IPFS.
    pub ipfs_gateways: Vec<Url>,
    /// Nostr relays (`wss://`, `ws://`), asked four at a time; a link's own
    /// relay hints first. They are clearnet hosts (or onions), reached as the
    /// clearnet is: directly, or through Tor exits with `clearnet_via_tor`.
    /// Empty disables Nostr.
    pub nostr_relays: Vec<Url>,
    /// Lets clearnet fetches reach loopback, private and link-local
    /// addresses. Off by default: otherwise any submitted URL turns the
    /// daemon into a proxy into the operator's LAN.
    pub allow_private_addresses: bool,
    /// Reach the clearnet through Tor exits instead of directly, for fetches
    /// and proxied connections alike. Hides the crawler's address at the cost
    /// of speed and of sites that block exits.
    pub clearnet_via_tor: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!("omnid/", env!("CARGO_PKG_VERSION")).into(),
            clearnet_timeout: Duration::from_secs(20),
            overlay_timeout: Duration::from_secs(90),
            tor_dir: None,
            i2p_proxies: vec![Url::parse(DEFAULT_I2P_PROXY).expect("valid default")],
            i2p_sam: Vec::new(),
            fproxies: vec![Url::parse(DEFAULT_FPROXY).expect("valid default")],
            ipfs_gateways: vec![Url::parse(DEFAULT_IPFS_GATEWAY).expect("valid default")],
            nostr_relays: DEFAULT_NOSTR_RELAYS.iter().map(|r| Url::parse(r).expect("valid default")).collect(),
            allow_private_addresses: false,
            clearnet_via_tor: false,
        }
    }
}

/// A byte stream from [`Gateway::connect`].
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

/// What to do with a response.
#[derive(Debug, Clone, Copy)]
pub struct FetchOptions {
    /// Bodies are truncated at this size.
    pub max_bytes: usize,
    /// Decides from the Content-Type header whether the body is read at all.
    /// A rejected body is never downloaded.
    pub accept: fn(Option<&str>) -> bool,
    /// Redirects to follow. Past the limit, the redirect is returned as
    /// [`Page::redirect`] instead of being followed.
    pub max_redirects: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self { max_bytes: 2 << 20, accept: |_| true, max_redirects: 5 }
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    /// Where the response came from, after the redirects that were followed.
    pub source: Source,
    pub status: u16,
    pub content_type: Option<String>,
    /// `None` for a redirect, or when [`FetchOptions::accept`] rejected the
    /// content type.
    pub body: Option<Bytes>,
    pub truncated: bool,
    /// The target of a redirect that was not followed.
    pub redirect: Option<Source>,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{network} is not available: {reason}")]
    Unavailable { network: Network, reason: String },
    #[error("refusing to fetch {0}: not a public address")]
    PrivateAddress(String),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("redirect to an unsupported location `{0}`")]
    BadRedirect(String),
    /// A redirect from an onion, I2P or Hyphanet site to the clearnet, which
    /// would go direct: refused, see [`Gateway::leaves_anonymity`].
    #[error("refusing to follow `{0}` from an anonymity network to the clearnet directly")]
    LeavesAnonymity(String),
    /// The message carries the whole cause chain: reqwest's top-level error
    /// alone ("error sending request") says nothing useful.
    #[error("{network}: {}", chain(.error.as_ref()))]
    Transport { network: Network, error: BoxError },
}

fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(e) = cause {
        message.push_str(": ");
        message.push_str(&e.to_string());
        cause = e.source();
    }
    message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterState {
    Ready,
    Starting,
    Unreachable,
    Disabled,
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouterStatus {
    pub network: Network,
    pub state: RouterState,
    pub detail: String,
}

pub struct Gateway {
    cfg: GatewayConfig,
    clearnet: reqwest::Client,
    i2p: Routers,
    sam: sam::Sam,
    fproxy: Routers,
    ipfs: Routers,
    tor: Option<tor::TorTransport>,
    /// Cancelled by [`Gateway::new_identity`], then replaced: the browser's
    /// streams opened before it end with it ([`Gateway::identity`]).
    identity: std::sync::Mutex<tokio_util::sync::CancellationToken>,
}

/// The TLS every connection omnid makes uses: Mozilla's roots (webpki-roots), checked by rustls
/// itself. Not the platform verifier reqwest would pick, which on Android needs the app's JNI
/// context (without it every HTTPS page panicked) and elsewhere may look up revocation on its own,
/// outside the gateway.
pub(crate) fn tls_config() -> Result<rustls::ClientConfig, BoxError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// [`tls_config`] for reqwest, which leaves ALPN to a config it is handed.
fn http_tls() -> Result<rustls::ClientConfig, BoxError> {
    let mut tls = tls_config()?;
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tls)
}

impl Gateway {
    /// Builds the transports. Tor starts bootstrapping in the background, so
    /// with `tor_dir` set this must be called inside a Tokio runtime.
    pub fn new(cfg: GatewayConfig) -> Result<Self, BoxError> {
        // arti's rustls backend uses the process-wide provider.
        let _already_installed = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let mut clearnet = reqwest::Client::builder()
            .tls_backend_preconfigured(http_tls()?)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(&cfg.user_agent)
            .timeout(cfg.clearnet_timeout)
            .connect_timeout(Duration::from_secs(10))
            .no_proxy();
        if !cfg.allow_private_addresses {
            clearnet = clearnet.dns_resolver(guard::PublicOnlyResolver);
        }
        let overlay = |proxy: Option<&Url>| -> Result<reqwest::Client, BoxError> {
            let mut builder = reqwest::Client::builder()
                .tls_backend_preconfigured(http_tls()?)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(&cfg.user_agent)
                .timeout(cfg.overlay_timeout)
                .connect_timeout(routers::CONNECT_TIMEOUT)
                .no_proxy();
            if let Some(proxy) = proxy {
                builder = builder.proxy(reqwest::Proxy::all(proxy.as_str())?);
            }
            Ok(builder.build()?)
        };
        let i2p = Routers::new(Network::I2p, &cfg.i2p_proxies, |proxy| overlay(Some(proxy)))?;
        let fproxy = Routers::new(Network::Hyphanet, &cfg.fproxies, |_| overlay(None))?;
        let ipfs = Routers::new(Network::Ipfs, &cfg.ipfs_gateways, |_| overlay(None))?;
        let tor = match &cfg.tor_dir {
            Some(dir) => Some(tor::TorTransport::new(dir, &cfg.user_agent)?),
            None => None,
        };
        let sam = sam::Sam::new(&cfg.i2p_sam)?;
        Ok(Self { clearnet: clearnet.build()?, i2p, sam, fproxy, ipfs, tor, cfg, identity: Default::default() })
    }

    pub async fn fetch(&self, source: &Source, opts: &FetchOptions) -> Result<Page, FetchError> {
        let mut current = source.clone();
        let mut hops = 0;
        loop {
            let response = self.fetch_once(&current, opts).await?;
            let redirect = match &response.location {
                Some(location) if http::is_redirect(response.status) => {
                    let location = self.localize(&current, location);
                    Some(current.join(location).ok_or_else(|| FetchError::BadRedirect(location.to_owned()))?)
                }
                _ => None,
            };
            match redirect {
                Some(next)
                    if hops < opts.max_redirects && !self.leaves_anonymity(current.transport(), next.transport()) =>
                {
                    hops += 1;
                    current = next;
                }
                redirect => {
                    return Ok(Page {
                        source: current,
                        status: response.status,
                        content_type: response.content_type,
                        body: response.body,
                        truncated: response.truncated,
                        redirect,
                    });
                }
            }
        }
    }

    /// The state of each network's router, for health checks and the UI.
    pub async fn status(&self) -> Vec<RouterStatus> {
        let tor = match &self.tor {
            Some(tor) => tor.status(),
            None => disabled(Network::Onion),
        };
        vec![
            RouterStatus {
                network: Network::Clearnet,
                state: RouterState::Ready,
                detail: if self.cfg.clearnet_via_tor { "through Tor" } else { "direct" }.into(),
            },
            tor,
            self.i2p_status().await,
            probe(&self.fproxy, Network::Hyphanet, "FProxy").await,
            probe(&self.ipfs, Network::Ipfs, "IPFS gateway").await,
            self.nostr_status(),
            self.small_web_status(Network::Gemini),
            self.small_web_status(Network::Gopher),
            RouterStatus {
                network: Network::Magnet,
                state: RouterState::Unsupported,
                detail: "magnet links are indexed; metadata needs the DHT module".into(),
            },
        ]
    }

    /// Nostr's relays, as configured: they are not probed, as a probe would be
    /// a connection to each every 30 seconds.
    fn nostr_status(&self) -> RouterStatus {
        let relays = &self.cfg.nostr_relays;
        if relays.is_empty() {
            return disabled(Network::Nostr);
        }
        let way = if self.cfg.clearnet_via_tor { " through Tor" } else { "" };
        let names: Vec<&str> = relays.iter().filter_map(Url::host_str).collect();
        RouterStatus {
            network: Network::Nostr,
            state: RouterState::Ready,
            detail: format!("relays{way}: {}", names.join(", ")),
        }
    }

    /// Gemini's and Gopher's way out: no router of their own, a connection to
    /// each server, as the clearnet's (an onion's through Tor, `.i2p`
    /// through I2P).
    fn small_web_status(&self, network: Network) -> RouterStatus {
        let way = if self.cfg.clearnet_via_tor { "through Tor" } else { "direct" };
        RouterStatus {
            network,
            state: RouterState::Ready,
            detail: format!("{way}; onions through Tor, .i2p through I2P"),
        }
    }

    /// The HTTP proxies' state, and the SAM bridges': whether the browser's
    /// sites get I2P identities of their own.
    async fn i2p_status(&self) -> RouterStatus {
        let (mut status, sam) = tokio::join!(probe(&self.i2p, Network::I2p, "i2pd HTTP proxy"), self.sam.describe());
        if status.state != RouterState::Disabled {
            status.detail = format!("{}; {sam}", status.detail);
        }
        status
    }

    async fn fetch_once(&self, source: &Source, opts: &FetchOptions) -> Result<http::RawResponse, FetchError> {
        let network = source.network();
        let timeout = match source.transport() {
            Network::Clearnet => self.cfg.clearnet_timeout,
            _ => self.cfg.overlay_timeout,
        };
        // A first bootstrap downloads the consensus and can outlast a request
        // timeout, so it is waited for outside of one.
        let over_tor = match source.transport() {
            Network::Onion => true,
            Network::Clearnet => self.cfg.clearnet_via_tor,
            _ => false,
        };
        if let (true, Some(tor)) = (over_tor, &self.tor) {
            tor.ready().await.map_err(|error| FetchError::Transport { network, error })?;
        }
        let fetch = async {
            match source {
                Source::Clearnet(url) if self.cfg.clearnet_via_tor => {
                    let tor = self.tor.as_ref().ok_or_else(|| unavailable(network, "clearnet via Tor needs Tor"))?;
                    tor.get(url, opts).await.map_err(|error| FetchError::Transport { network, error })
                }
                Source::Clearnet(url) => {
                    if !self.cfg.allow_private_addresses && !guard::host_is_public(url) {
                        return Err(FetchError::PrivateAddress(url.to_string()));
                    }
                    http::reqwest_get(&self.clearnet, url.as_str(), opts)
                        .await
                        .map_err(|e| FetchError::Transport { network, error: e.into() })
                }
                Source::Onion(url) => {
                    let tor = self.tor.as_ref().ok_or_else(|| unavailable(network, "Tor is disabled"))?;
                    tor.get(url, opts).await.map_err(|error| FetchError::Transport { network, error })
                }
                Source::I2p(url) => {
                    let page = |_: &Url| Ok(url.clone());
                    via_routers(&self.i2p, network, "no I2P proxy configured", page, any_answer, opts).await
                }
                Source::Hyphanet(key) => {
                    let page =
                        |node: &Url| node.join(key).map_err(|e| FetchError::Transport { network, error: e.into() });
                    via_routers(&self.fproxy, network, "no FProxy configured", page, any_answer, opts).await
                }
                Source::Ipfs(_) => {
                    let path = source.gateway_path().unwrap_or_default();
                    let page = |gateway: &Url| {
                        gateway.join(&path).map_err(|e| FetchError::Transport { network, error: e.into() })
                    };
                    via_routers(&self.ipfs, network, "no IPFS gateway configured", page, ipfs_answer, opts).await
                }
                Source::Gemini(_) | Source::Gopher(_) => {
                    Ok(self.small_web_on(source, opts, Circuits::Shared, false).await?.into_raw())
                }
                Source::Nostr(_) => Err(unavailable(network, "Nostr events come from relays: Gateway::nostr")),
                Source::Magnet(_) => Err(unavailable(
                    network,
                    "fetching torrent metadata (BEP 9) needs the DHT module, which is not built yet",
                )),
            }
        };
        tokio::time::timeout(timeout, fetch).await.map_err(|_| FetchError::Timeout(timeout))?
    }

    /// A Gemini capsule's or a Gopher hole's answer for `source`. With
    /// [`Route::Direct`], on the network its host is on, as
    /// [`Gateway::connect`] routes; with [`Route::Browser`], as the built-in
    /// browser's streams go ([`Gateway::connect_browser`]): never directly.
    /// Redirects are left to the caller, in [`SmallWeb::Redirect`].
    pub async fn small_web(&self, source: &Source, opts: &FetchOptions, route: &Route) -> Result<SmallWeb, FetchError> {
        let (circuits, browser) = match route {
            Route::Direct => (Circuits::Shared, false),
            Route::Browser { site } => (Circuits::Site(site), true),
        };
        let timeout = self.cfg.overlay_timeout;
        tokio::time::timeout(timeout, self.small_web_on(source, opts, circuits, browser))
            .await
            .map_err(|_| FetchError::Timeout(timeout))?
    }

    async fn small_web_on(
        &self,
        source: &Source,
        opts: &FetchOptions,
        circuits: Circuits<'_>,
        browser: bool,
    ) -> Result<SmallWeb, FetchError> {
        let network = source.network();
        let transport = |error: BoxError| FetchError::Transport { network, error };
        let url = source.small_web_url().ok_or_else(|| unavailable(network, "not a Gemini or Gopher address"))?;
        let host = url.host_str().ok_or_else(|| transport("an address without a host".into()))?;
        let default = if network == Network::Gemini { 1965 } else { 70 };
        let port = url.port().unwrap_or(default);
        // A Gopher item's type is known before asking: a binary is never
        // asked for, and a type the caller refuses is not connected for.
        if let Some((item_type, _)) = source.gopher_item() {
            let content_type = gopher::content_type(item_type)
                .ok_or_else(|| transport(format!("Gopher item type {item_type:?} is not text").into()))?;
            if !(opts.accept)(Some(content_type)) {
                return Ok(SmallWeb::Page { content_type: content_type.to_owned(), body: None, truncated: false });
            }
        }
        let stream = if browser {
            self.connect_browser_on(host, port, circuits).await?
        } else {
            self.connect_on(host, port, circuits).await?
        };
        match source.gopher_item() {
            Some((item_type, selector)) => gopher::get(stream, item_type, &selector, opts).await,
            None => gemini::get(stream, url, opts).await,
        }
        .map_err(transport)
    }

    /// Opens a raw stream to `host:port` over the network its name implies:
    /// `.onion` through Tor, `.i2p` through i2pd, [`HYPHANET_HOST`] to the
    /// Hyphanet node, anything else directly (or through Tor with
    /// `clearnet_via_tor`). This is what the SOCKS proxy
    /// serves. Tor streams buffer writes: flush after writing a request.
    pub async fn connect(&self, host: &str, port: u16) -> Result<BoxStream, FetchError> {
        self.connect_on(host, port, Circuits::Shared).await
    }

    /// [`Gateway::connect`], with a stream through Tor on the circuits
    /// `circuits` picks.
    pub(crate) async fn connect_on(
        &self,
        host: &str,
        port: u16,
        circuits: Circuits<'_>,
    ) -> Result<BoxStream, FetchError> {
        let (host, network) = stream_target(host);
        let tor = || self.tor.as_ref().ok_or_else(|| unavailable(network, "Tor is disabled"));
        let transport = |error: BoxError| FetchError::Transport { network, error };
        let over_tor = network == Network::Onion || (network == Network::Clearnet && self.cfg.clearnet_via_tor);
        if over_tor {
            tor()?.ready().await.map_err(transport)?;
        }
        let timeout = if network == Network::Clearnet && !over_tor {
            self.cfg.clearnet_timeout
        } else {
            self.cfg.overlay_timeout
        };
        let connect = async {
            if over_tor {
                return Ok(Box::new(tor()?.stream_on(&host, port, circuits).await.map_err(transport)?) as BoxStream);
            }
            match network {
                Network::I2p => self.i2p_tunnel(&host, port).await,
                Network::Hyphanet => self.fproxy_stream(port).await,
                _ => self.direct(&host, port).await,
            }
        };
        tokio::time::timeout(timeout, connect).await.map_err(|_| FetchError::Timeout(timeout))?
    }

    /// A stream for the built-in browser, which is on `site`: `.onion` and
    /// the clearnet through Tor, on circuits no other site and not the
    /// crawler uses; `.i2p` through a SAM bridge on an I2P identity of the
    /// site's own, or through i2pd's HTTP proxy when no bridge answers;
    /// [`HYPHANET_HOST`] to the Hyphanet node. Never a direct connection to anywhere else, whatever
    /// `clearnet_via_tor` says: without Tor the clearnet is unavailable.
    pub async fn connect_browser(&self, host: &str, port: u16, site: &str) -> Result<BoxStream, FetchError> {
        self.connect_browser_on(host, port, Circuits::Site(site)).await
    }

    /// [`Gateway::connect_browser`], with a stream through Tor on the
    /// circuits `circuits` picks.
    pub(crate) async fn connect_browser_on(
        &self,
        host: &str,
        port: u16,
        circuits: Circuits<'_>,
    ) -> Result<BoxStream, FetchError> {
        let (host, network) = stream_target(host);
        let timeout = self.cfg.overlay_timeout;
        let connect = async {
            match network {
                Network::I2p => return self.i2p_isolated(&host, port, circuits).await,
                Network::Hyphanet => return self.fproxy_stream(port).await,
                _ => {}
            }
            let tor = self.tor.as_ref().ok_or_else(|| unavailable(network, "Tor is disabled"))?;
            let transport = |error: BoxError| FetchError::Transport { network, error };
            tor.ready().await.map_err(transport)?;
            Ok(Box::new(tor.stream_on(&host, port, circuits).await.map_err(transport)?) as BoxStream)
        };
        tokio::time::timeout(timeout, connect).await.map_err(|_| FetchError::Timeout(timeout))?
    }

    /// Whether a step from `from` to `to` (a redirect, say) would leave an
    /// anonymity network for a fetch from this machine's own address: a direct
    /// clearnet connection, or IPFS ([`Gateway::reaches_directly`]). A site on
    /// an onion, I2P or Hyphanet, or a magnet, could point at a clearnet
    /// address or a CID its author watches and so learn who followed it; only
    /// with `clearnet_via_tor` does the clearnet go through Tor too.
    /// [`Gateway::fetch`] leaves such a redirect unfollowed, whatever
    /// `max_redirects` says.
    pub fn leaves_anonymity(&self, from: Network, to: Network) -> bool {
        !self.reaches_directly(from) && self.reaches_directly(to)
    }

    /// Whether a fetch on `network` comes from this machine's own address, or
    /// one of the owner's: the clearnet and Nostr's relays without
    /// `clearnet_via_tor`, and IPFS,
    /// whose node asks its peers for the content itself. An onion site linking
    /// a CID of its own could learn who fetched it as easily as from a
    /// clearnet address.
    ///
    /// It takes a [`Source::transport`]: a Gemini capsule or a Gopher hole is
    /// reached on the network its host is on. Asked about Gemini or Gopher
    /// themselves, it answers as for the clearnet.
    pub fn reaches_directly(&self, network: Network) -> bool {
        match network {
            Network::Clearnet | Network::Nostr | Network::Gemini | Network::Gopher => !self.cfg.clearnet_via_tor,
            Network::Ipfs => true,
            Network::Onion | Network::I2p | Network::Hyphanet | Network::Magnet => false,
        }
    }

    /// The best Hyphanet node's FProxy, if one is configured.
    pub fn fproxy(&self) -> Option<&Url> {
        self.fproxy.best()
    }

    /// The best IPFS gateway, if one is configured.
    pub fn ipfs_gateway(&self) -> Option<&Url> {
        self.ipfs.best()
    }

    /// New Tor circuits and I2P identities for every site the browser visits
    /// from now on, and the end of the streams it opened before: a page
    /// reopened soon after must not come back on a kept-alive connection over
    /// the old circuits.
    pub fn new_identity(&self) {
        if let Some(tor) = &self.tor {
            tor.new_identity();
        }
        self.sam.reset();
        let old = std::mem::take(&mut *self.identity.lock().expect("identity lock poisoned"));
        old.cancel();
    }

    /// The browser's current identity: cancelled when [`Gateway::new_identity`]
    /// replaces it, so streams opened under it can end then.
    pub fn identity(&self) -> tokio_util::sync::CancellationToken {
        self.identity.lock().expect("identity lock poisoned").clone()
    }

    /// A direct TCP connection, refusing non-public addresses unless allowed;
    /// the addresses connected to are the ones checked.
    async fn direct(&self, host: &str, port: u16) -> Result<BoxStream, FetchError> {
        let network = Network::Clearnet;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((host, port))
                .await
                .map_err(|e| FetchError::Transport { network, error: e.into() })?
                .collect(),
        };
        let allowed: Vec<_> =
            addrs.into_iter().filter(|a| self.cfg.allow_private_addresses || is_public_ip(a.ip())).collect();
        if allowed.is_empty() {
            return Err(FetchError::PrivateAddress(format!("{host}:{port}")));
        }
        let stream = tokio::net::TcpStream::connect(&allowed[..])
            .await
            .map_err(|e| FetchError::Transport { network, error: e.into() })?;
        Ok(Box::new(stream))
    }

    /// A stream to a Hyphanet node's FProxy, for [`HYPHANET_HOST`] on port
    /// 80: the nodes omnid was given are where Hyphanet pages come from, as
    /// i2pd is for `.i2p`. Plain HTTP only; an HTTPS attempt is refused.
    async fn fproxy_stream(&self, port: u16) -> Result<BoxStream, FetchError> {
        let network = Network::Hyphanet;
        if port != 80 {
            let error = format!("Hyphanet pages are http://{HYPHANET_HOST}/ on port 80, not {port}");
            return Err(FetchError::Transport { network, error: error.into() });
        }
        self.fproxy
            .race(routers::STAGGER, |node| async move {
                if node.scheme() != "http" {
                    return Err(unavailable(network, "the browser reaches a Hyphanet node over http only"));
                }
                Ok(Box::new(routers::connect(network, &node).await?) as BoxStream)
            })
            .await
    }

    /// An HTTP CONNECT tunnel through an i2pd HTTP proxy. With several routers,
    /// the first to open the tunnel wins: one may find the site sooner, or at
    /// all, when the other cannot.
    async fn i2p_tunnel(&self, host: &str, port: u16) -> Result<BoxStream, FetchError> {
        let target = format!("{host}:{port}");
        self.i2p.race(routers::STAGGER, |proxy| i2p_tunnel(proxy, target.clone())).await
    }

    /// A stream to an eepsite on the I2P identity `circuits` picks: a site's,
    /// or one of its own, through a SAM bridge. The crawler's, and every
    /// stream's when no bridge answers, is the HTTP proxy's.
    async fn i2p_isolated(&self, host: &str, port: u16, circuits: Circuits<'_>) -> Result<BoxStream, FetchError> {
        let key = match circuits {
            Circuits::Site(site) => Some(site),
            Circuits::Own(_) => None,
            Circuits::Shared => return self.i2p_tunnel(host, port).await,
        };
        match self.sam.stream(host, port, key).await? {
            Some(stream) => Ok(Box::new(stream)),
            None => self.i2p_tunnel(host, port).await,
        }
    }

    /// FProxy and IPFS gateways may redirect to their own absolute URLs;
    /// strip the origin so the redirect resolves to a key or a gateway path
    /// rather than to a loopback clearnet URL.
    fn localize<'a>(&self, current: &Source, location: &'a str) -> &'a str {
        let routers = match current {
            Source::Hyphanet(_) => &self.fproxy,
            Source::Ipfs(_) => &self.ipfs,
            _ => return location,
        };
        routers.urls().find_map(|node| location.strip_prefix(node.as_str().trim_end_matches('/'))).unwrap_or(location)
    }
}

/// A stream target's host, lower case without a trailing dot, and the network
/// its name puts it on.
fn stream_target(host: &str) -> (String, Network) {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let network = if host.ends_with(".onion") {
        Network::Onion
    } else if host.ends_with(".i2p") {
        Network::I2p
    } else if host == HYPHANET_HOST {
        Network::Hyphanet
    } else {
        Network::Clearnet
    };
    (host, network)
}

fn unavailable(network: Network, reason: &str) -> FetchError {
    FetchError::Unavailable { network, reason: reason.to_owned() }
}

fn disabled(network: Network) -> RouterStatus {
    RouterStatus { network, state: RouterState::Disabled, detail: "disabled in config".into() }
}

/// A refused connection to a router means the router is not running, which is
/// worth saying plainly instead of as a socket error.
fn router_error(network: Network, router: &Url, error: reqwest::Error) -> FetchError {
    if error.is_connect() {
        FetchError::Unavailable { network, reason: format!("nothing is listening on {router}; is the router running?") }
    } else {
        FetchError::Transport { network, error: error.into() }
    }
}

/// Error pages i2pd makes itself when it cannot reach a site; another router
/// may reach it.
fn is_gateway_error(status: u16) -> bool {
    matches!(status, 500 | 502 | 503 | 504)
}

/// Whether an answer from a router is one: any is, from i2pd and FProxy.
fn any_answer(_: &http::RawResponse) -> Result<(), &'static str> {
    Ok(())
}

/// A page from an IPFS gateway carries its headers. Without them, whatever
/// else listens on the port (8080 is a popular one) answered, and its pages
/// are not IPFS content.
fn ipfs_answer(response: &http::RawResponse) -> Result<(), &'static str> {
    if (200..300).contains(&response.status) && !response.ipfs_gateway {
        return Err("without an IPFS gateway's headers (Ipfs-Uri, X-Ipfs-Roots); is something else on that port?");
    }
    Ok(())
}

/// A GET through a network's routers, best first, at the URL `page` gives for
/// each. The next router is asked when one cannot be reached, answers with a
/// gateway error, or gives an answer `genuine` refuses (that router rests);
/// with none doing better, the last answer stands.
async fn via_routers(
    routers: &Routers,
    network: Network,
    none: &str,
    page: impl Fn(&Url) -> Result<Url, FetchError>,
    genuine: fn(&http::RawResponse) -> Result<(), &'static str>,
    opts: &FetchOptions,
) -> Result<http::RawResponse, FetchError> {
    let mut last = Err(unavailable(network, none));
    for router in routers.order() {
        if let Err(error) = routers::askable(network, &router.url) {
            router.failed();
            last = Err(error);
            continue;
        }
        let url = page(&router.url)?;
        match http::reqwest_get(&router.client, url.as_str(), opts).await {
            Ok(response) => match genuine(&response) {
                Err(why) => {
                    router.failed();
                    last = Err(unavailable(network, &format!("{} answered {why}", router.url)));
                }
                Ok(()) => {
                    router.succeeded();
                    if !is_gateway_error(response.status) {
                        return Ok(response);
                    }
                    last = Ok(response);
                }
            },
            Err(e) => {
                let error = router_error(network, &router.url, e);
                if routers::is_unreachable(&error) {
                    router.failed();
                }
                last = Err(error);
            }
        }
    }
    last
}

/// An HTTP CONNECT tunnel to `target` through the i2pd HTTP proxy at `proxy`.
async fn i2p_tunnel(proxy: Url, target: String) -> Result<BoxStream, FetchError> {
    let network = Network::I2p;
    let mut stream = routers::connect(network, &proxy).await?;
    let transport = |error: BoxError| FetchError::Transport { network, error };
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(|e| transport(e.into()))?;
    // Read the response head a byte at a time so nothing after it is
    // consumed: those bytes belong to the tunnel.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8 << 10 {
            return Err(transport("oversized CONNECT response".into()));
        }
        head.push(stream.read_u8().await.map_err(|e| transport(e.into()))?);
    }
    let status = String::from_utf8_lossy(&head);
    let status = status.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(transport(format!("i2pd refused the tunnel: {status}").into()));
    }
    Ok(Box::new(stream))
}

/// Each router's state; the network is ready when one of them is. What a
/// probe finds also decides which router requests go to first.
async fn probe(routers: &Routers, network: Network, what: &str) -> RouterStatus {
    let all = routers.all();
    if all.is_empty() {
        return disabled(network);
    }
    let mut probes = tokio::task::JoinSet::new();
    for (i, router) in all.iter().enumerate() {
        let url = router.url.clone();
        probes.spawn(async move { (i, routers::connect(network, &url).await.is_ok()) });
    }
    let mut up = vec![false; all.len()];
    while let Some(Ok((i, ok))) = probes.join_next().await {
        up[i] = ok;
    }
    let mut details = Vec::new();
    for (router, ok) in all.iter().zip(&up) {
        if *ok {
            router.succeeded();
            details.push(format!("{what} at {}", router.url));
        } else {
            router.failed();
            let why = if routers::askable(network, &router.url).is_err() { " (not on its tailnet now)" } else { "" };
            details.push(format!("no {what} at {}{why}", router.url));
        }
    }
    let state = if up.contains(&true) { RouterState::Ready } else { RouterState::Unreachable };
    RouterStatus { network, state, detail: details.join("; ") }
}

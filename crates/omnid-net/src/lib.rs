//! The multi-network fetch layer. [`Gateway::fetch`] takes any [`Source`]
//! and routes it to the transport that can reach it:
//!
//! | Network  | Transport                                           |
//! |----------|-----------------------------------------------------|
//! | clearnet | reqwest, with a resolver that refuses private IPs   |
//! | onion    | arti, in-process, HTTP/1.1 over a Tor stream        |
//! | i2p      | i2pd's HTTP proxy                                   |
//! | hyphanet | the node's FProxy                                   |
//! | magnet   | not yet: needs the DHT module                       |
//!
//! Redirects are resolved here, against the source that issued them, so a
//! clearnet page that redirects to an onion is followed over Tor.

mod guard;
mod http;
mod tor;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use omnid_core::{Network, Source};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

pub use guard::is_public_ip;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub user_agent: String,
    pub clearnet_timeout: Duration,
    /// Overlay networks build circuits or tunnels per request and are slow.
    pub overlay_timeout: Duration,
    /// Where arti keeps its state and directory cache. `None` disables Tor.
    pub tor_dir: Option<PathBuf>,
    /// i2pd's HTTP proxy. `None` disables I2P.
    pub i2p_proxy: Option<Url>,
    /// The Hyphanet node's FProxy. `None` disables Hyphanet.
    pub fproxy: Option<Url>,
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
            i2p_proxy: Some(Url::parse("http://127.0.0.1:4444").expect("valid default")),
            fproxy: Some(Url::parse("http://127.0.0.1:8888").expect("valid default")),
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
    i2p: Option<reqwest::Client>,
    fproxy: Option<reqwest::Client>,
    tor: Option<tor::TorTransport>,
}

impl Gateway {
    /// Builds the transports. Tor starts bootstrapping in the background, so
    /// with `tor_dir` set this must be called inside a Tokio runtime.
    pub fn new(cfg: GatewayConfig) -> Result<Self, BoxError> {
        // arti's rustls backend uses the process-wide provider.
        let _already_installed = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let mut clearnet = reqwest::Client::builder()
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
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(&cfg.user_agent)
                .timeout(cfg.overlay_timeout)
                .no_proxy();
            if let Some(proxy) = proxy {
                builder = builder.proxy(reqwest::Proxy::all(proxy.as_str())?);
            }
            Ok(builder.build()?)
        };
        let i2p = match &cfg.i2p_proxy {
            Some(proxy) => Some(overlay(Some(proxy))?),
            None => None,
        };
        let fproxy = match &cfg.fproxy {
            Some(_) => Some(overlay(None)?),
            None => None,
        };
        let tor = match &cfg.tor_dir {
            Some(dir) => Some(tor::TorTransport::new(dir, &cfg.user_agent)?),
            None => None,
        };
        Ok(Self { clearnet: clearnet.build()?, i2p, fproxy, tor, cfg })
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
                Some(next) if hops < opts.max_redirects => {
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
            RouterStatus { network: Network::Clearnet, state: RouterState::Ready, detail: "direct".into() },
            tor,
            probe(Network::I2p, self.cfg.i2p_proxy.as_ref(), "i2pd HTTP proxy").await,
            probe(Network::Hyphanet, self.cfg.fproxy.as_ref(), "FProxy").await,
            RouterStatus {
                network: Network::Magnet,
                state: RouterState::Unsupported,
                detail: "magnet links are indexed; metadata needs the DHT module".into(),
            },
        ]
    }

    async fn fetch_once(&self, source: &Source, opts: &FetchOptions) -> Result<http::RawResponse, FetchError> {
        let network = source.network();
        let timeout = match network {
            Network::Clearnet => self.cfg.clearnet_timeout,
            _ => self.cfg.overlay_timeout,
        };
        // A first bootstrap downloads the consensus and can outlast a request
        // timeout, so it is waited for outside of one.
        let over_tor = match source {
            Source::Onion(_) => true,
            Source::Clearnet(_) => self.cfg.clearnet_via_tor,
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
                    let (client, proxy) = self
                        .i2p
                        .as_ref()
                        .zip(self.cfg.i2p_proxy.as_ref())
                        .ok_or_else(|| unavailable(network, "no I2P proxy configured"))?;
                    http::reqwest_get(client, url.as_str(), opts).await.map_err(|e| router_error(network, proxy, e))
                }
                Source::Hyphanet(key) => {
                    let (client, fproxy) = self
                        .fproxy
                        .as_ref()
                        .zip(self.cfg.fproxy.as_ref())
                        .ok_or_else(|| unavailable(network, "no FProxy configured"))?;
                    let url = fproxy.join(key).map_err(|e| FetchError::Transport { network, error: e.into() })?;
                    http::reqwest_get(client, url.as_str(), opts).await.map_err(|e| router_error(network, fproxy, e))
                }
                Source::Magnet(_) => Err(unavailable(
                    network,
                    "fetching torrent metadata (BEP 9) needs the DHT module, which is not built yet",
                )),
            }
        };
        tokio::time::timeout(timeout, fetch).await.map_err(|_| FetchError::Timeout(timeout))?
    }

    /// Opens a raw stream to `host:port` over the network its name implies:
    /// `.onion` through Tor, `.i2p` through i2pd, anything else directly (or
    /// through Tor with `clearnet_via_tor`). This is what the SOCKS proxy
    /// serves. Tor streams buffer writes: flush after writing a request.
    pub async fn connect(&self, host: &str, port: u16) -> Result<BoxStream, FetchError> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let network = if host.ends_with(".onion") {
            Network::Onion
        } else if host.ends_with(".i2p") {
            Network::I2p
        } else {
            Network::Clearnet
        };
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
                return Ok(Box::new(tor()?.stream(&host, port).await.map_err(transport)?) as BoxStream);
            }
            match network {
                Network::I2p => self.i2p_tunnel(&host, port).await,
                _ => self.direct(&host, port).await,
            }
        };
        tokio::time::timeout(timeout, connect).await.map_err(|_| FetchError::Timeout(timeout))?
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

    /// An HTTP CONNECT tunnel through i2pd's HTTP proxy.
    async fn i2p_tunnel(&self, host: &str, port: u16) -> Result<BoxStream, FetchError> {
        let network = Network::I2p;
        let proxy = self.cfg.i2p_proxy.as_ref().ok_or_else(|| unavailable(network, "no I2P proxy configured"))?;
        let (Some(proxy_host), Some(proxy_port)) = (proxy.host_str(), proxy.port_or_known_default()) else {
            return Err(unavailable(network, "bad I2P proxy URL"));
        };
        let mut stream =
            tokio::net::TcpStream::connect((proxy_host, proxy_port)).await.map_err(|_| FetchError::Unavailable {
                network,
                reason: format!("nothing is listening on {proxy}; is the router running?"),
            })?;
        let transport = |error: BoxError| FetchError::Transport { network, error };
        let request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
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

    /// FProxy may redirect to its own absolute URL; strip the origin so the
    /// redirect resolves to a key rather than to a loopback clearnet URL.
    fn localize<'a>(&self, current: &Source, location: &'a str) -> &'a str {
        match (current, &self.cfg.fproxy) {
            (Source::Hyphanet(_), Some(fproxy)) => {
                location.strip_prefix(fproxy.as_str().trim_end_matches('/')).unwrap_or(location)
            }
            _ => location,
        }
    }
}

fn unavailable(network: Network, reason: &str) -> FetchError {
    FetchError::Unavailable { network, reason: reason.to_owned() }
}

fn disabled(network: Network) -> RouterStatus {
    RouterStatus { network, state: RouterState::Disabled, detail: "disabled in config".into() }
}

/// A refused connection to a local router means the router is not running,
/// which is worth saying plainly instead of as a socket error.
fn router_error(network: Network, router: &Url, error: reqwest::Error) -> FetchError {
    if error.is_connect() {
        FetchError::Unavailable { network, reason: format!("nothing is listening on {router}; is the router running?") }
    } else {
        FetchError::Transport { network, error: error.into() }
    }
}

async fn probe(network: Network, router: Option<&Url>, what: &str) -> RouterStatus {
    let Some(router) = router else {
        return disabled(network);
    };
    let (Some(host), Some(port)) = (router.host_str(), router.port_or_known_default()) else {
        return RouterStatus { network, state: RouterState::Unreachable, detail: format!("bad {what} URL {router}") };
    };
    let connect = tokio::net::TcpStream::connect((host, port));
    match tokio::time::timeout(Duration::from_secs(1), connect).await {
        Ok(Ok(_)) => RouterStatus { network, state: RouterState::Ready, detail: format!("{what} at {router}") },
        _ => RouterStatus { network, state: RouterState::Unreachable, detail: format!("no {what} at {router}") },
    }
}

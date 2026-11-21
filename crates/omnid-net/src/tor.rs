use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arti_client::config::{BoolOrAuto, TorClientConfigBuilder};
use arti_client::{BootstrapBehavior, DataStream, IsolationToken, StreamPrefs, TorClient};
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;
use tor_rtcompat::PreferredRuntime;
use url::Url;

use crate::http::{self, RawResponse, Sender};
use crate::{BoxError, FetchOptions, RouterState, RouterStatus};

/// Tor through arti, running inside the daemon: no separate Tor process.
/// Keep-alive connections are dropped after this long idle; servers close
/// theirs after a minute or more.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// A site is fetched one page at a time, so one spare is plenty.
const MAX_IDLE_PER_SITE: usize = 2;
/// Sites with circuits of their own; past this many, every site starts over
/// on new circuits rather than the map growing without bound.
const MAX_ISOLATED_SITES: usize = 4_096;

pub(crate) struct TorTransport {
    client: Arc<TorClient<PreferredRuntime>>,
    prefs: StreamPrefs,
    tls: TlsConnector,
    user_agent: String,
    pool: Pool,
    isolation: Isolation,
}

/// Which circuits a stream goes on.
#[derive(Clone, Copy)]
pub(crate) enum Circuits<'a> {
    /// The crawler's, which every stream that asks for nothing else shares.
    Shared,
    /// Those of a site the browser is on (see [`Isolation`]).
    Site(&'a str),
    /// Circuits of their own, which only streams with this token use: each of
    /// a download's connections gets one.
    Own(IsolationToken),
}

impl Circuits<'_> {
    pub(crate) fn own() -> Self {
        Circuits::Own(IsolationToken::new())
    }
}

/// One isolation token per site the browser is on, as Tor Browser does:
/// streams for different sites never share a circuit, so an exit or an
/// onion service cannot link them by circuit.
#[derive(Default)]
struct Isolation(Mutex<HashMap<String, IsolationToken>>);

impl Isolation {
    fn token(&self, site: &str) -> IsolationToken {
        let mut tokens = self.0.lock().expect("isolation lock poisoned");
        if tokens.len() >= MAX_ISOLATED_SITES && !tokens.contains_key(site) {
            tokens.clear();
        }
        *tokens.entry(site.to_owned()).or_insert_with(IsolationToken::new)
    }

    /// A fresh token for `site`, replacing the one it had.
    fn renew(&self, site: &str) -> IsolationToken {
        let token = IsolationToken::new();
        self.0.lock().expect("isolation lock poisoned").insert(site.to_owned(), token);
        token
    }

    fn reset(&self) {
        self.0.lock().expect("isolation lock poisoned").clear();
    }
}

/// Idle keep-alive connections by `scheme://host:port`. Reusing one skips
/// opening a Tor stream (a round trip through the circuit) and, for HTTPS,
/// the TLS handshake.
#[derive(Default)]
struct Pool(Mutex<HashMap<String, Vec<(Sender, Instant)>>>);

impl Pool {
    fn take(&self, key: &str) -> Option<Sender> {
        let mut idle = self.0.lock().expect("pool lock poisoned");
        let senders = idle.get_mut(key)?;
        while let Some((sender, since)) = senders.pop() {
            if since.elapsed() < IDLE_TIMEOUT && sender.is_ready() {
                return Some(sender);
            }
        }
        None
    }

    fn put(&self, key: String, sender: Sender) {
        let mut idle = self.0.lock().expect("pool lock poisoned");
        idle.retain(|_, senders| {
            senders.retain(|(sender, since)| since.elapsed() < IDLE_TIMEOUT && !sender.is_closed());
            !senders.is_empty()
        });
        let senders = idle.entry(key).or_default();
        if senders.len() < MAX_IDLE_PER_SITE {
            senders.push((sender, Instant::now()));
        }
    }
}

impl TorTransport {
    pub(crate) fn new(dir: &Path, user_agent: &str) -> Result<Self, BoxError> {
        let config = TorClientConfigBuilder::from_directories(dir.join("state"), dir.join("cache")).build()?;
        let client = TorClient::builder()
            .config(config)
            .bootstrap_behavior(BootstrapBehavior::OnDemand)
            .create_unbootstrapped()?;

        // Bootstrap now rather than on the first request, so the first
        // crawl does not pay for downloading the consensus.
        let bootstrapping = client.clone();
        tokio::spawn(async move {
            match bootstrapping.bootstrap().await {
                Ok(()) => tracing::info!("tor: bootstrapped"),
                Err(e) => tracing::warn!("tor: bootstrap failed, will retry on demand: {e}"),
            }
        });

        let mut prefs = StreamPrefs::new();
        prefs.connect_to_onion_services(BoolOrAuto::Explicit(true));
        Ok(Self {
            client,
            prefs,
            tls: tls_connector()?,
            user_agent: user_agent.to_owned(),
            pool: Pool::default(),
            isolation: Isolation::default(),
        })
    }

    pub(crate) fn status(&self) -> RouterStatus {
        let status = self.client.bootstrap_status();
        let network = omnid_core::Network::Onion;
        if status.ready_for_traffic() {
            RouterStatus { network, state: RouterState::Ready, detail: "arti, in-process".into() }
        } else {
            RouterStatus {
                network,
                state: RouterState::Starting,
                detail: format!("bootstrapping {:.0}%", status.as_frac() * 100.0),
            }
        }
    }

    /// Returns once Tor can carry traffic, joining a bootstrap in progress.
    pub(crate) async fn ready(&self) -> Result<(), BoxError> {
        if self.client.bootstrap_status().ready_for_traffic() {
            return Ok(());
        }
        Ok(self.client.bootstrap().await?)
    }

    /// A stream to `host:port` through Tor: an onion service, or the
    /// clearnet through an exit.
    pub(crate) async fn stream(&self, host: &str, port: u16) -> Result<DataStream, BoxError> {
        Ok(self.client.connect_with_prefs((host, port), &self.prefs).await?)
    }

    /// A stream on circuits that only streams for `site` use, and never the
    /// crawler's. If it fails, the site's circuit may be what failed (an exit
    /// that stopped resolving or reaching the host keeps failing every stream
    /// put on it), so it is tried once more on a new one.
    pub(crate) async fn isolated_stream(&self, host: &str, port: u16, site: &str) -> Result<DataStream, BoxError> {
        let connect = |token: IsolationToken| {
            let mut prefs = self.prefs.clone();
            prefs.set_isolation(token);
            async move { self.client.connect_with_prefs((host, port), &prefs).await }
        };
        match connect(self.isolation.token(site)).await {
            Ok(stream) => Ok(stream),
            Err(e) => {
                tracing::debug!(host, "tor: {e}; trying a new circuit for {site}");
                Ok(connect(self.isolation.renew(site)).await?)
            }
        }
    }

    /// A stream on the circuits `circuits` picks.
    pub(crate) async fn stream_on(
        &self,
        host: &str,
        port: u16,
        circuits: Circuits<'_>,
    ) -> Result<DataStream, BoxError> {
        match circuits {
            Circuits::Shared => self.stream(host, port).await,
            Circuits::Site(site) => self.isolated_stream(host, port, site).await,
            Circuits::Own(token) => {
                let mut prefs = self.prefs.clone();
                prefs.set_isolation(token);
                Ok(self.client.connect_with_prefs((host, port), &prefs).await?)
            }
        }
    }

    /// Every site gets new circuits from its next stream on.
    pub(crate) fn new_identity(&self) {
        self.isolation.reset();
    }

    pub(crate) async fn get(&self, url: &Url, opts: &FetchOptions) -> Result<RawResponse, BoxError> {
        let host = url.host_str().ok_or("URL has no host")?;
        let port = url.port_or_known_default().ok_or("URL has no port")?;
        let key = format!("{}://{host}:{port}", url.scheme());
        if let Some(mut sender) = self.pool.take(&key) {
            // GET is idempotent: if the server closed the idle connection in
            // the meantime, the request is retried once on a fresh one.
            if let Ok((response, reusable)) = http::get(&mut sender, url, &self.user_agent, opts).await {
                if reusable {
                    self.pool.put(key, sender);
                }
                return Ok(response);
            }
        }
        let mut sender = self.connect(url.scheme(), host, port).await?;
        let (response, reusable) = http::get(&mut sender, url, &self.user_agent, opts).await?;
        if reusable {
            self.pool.put(key, sender);
        }
        Ok(response)
    }

    async fn connect(&self, scheme: &str, host: &str, port: u16) -> Result<Sender, BoxError> {
        let stream = self.stream(host, port).await?;
        if scheme == "https" {
            let name = ServerName::try_from(host.to_owned())?;
            http::handshake(self.tls.connect(name, stream).await?).await
        } else {
            http::handshake(stream).await
        }
    }
}

fn tls_connector() -> Result<TlsConnector, BoxError> {
    Ok(TlsConnector::from(Arc::new(crate::tls_config()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_sites_from_each_other_until_a_new_identity() {
        let isolation = Isolation::default();
        let first = isolation.token("example.com");
        assert_eq!(isolation.token("example.com"), first, "one site keeps its circuits");
        assert_ne!(isolation.token("example.org"), first, "another site gets its own");
        let renewed = isolation.renew("example.com");
        assert_ne!(renewed, first, "a failing site moves to a new circuit");
        assert_eq!(isolation.token("example.com"), renewed, "and stays on it");
        isolation.reset();
        assert_ne!(isolation.token("example.com"), renewed, "a new identity gets new circuits");
    }

    #[test]
    fn forgets_every_site_rather_than_growing_without_bound() {
        let isolation = Isolation::default();
        let first = isolation.token("site0");
        for i in 1..MAX_ISOLATED_SITES {
            isolation.token(&format!("site{i}"));
        }
        assert_eq!(isolation.token("site0"), first, "still remembered at the cap");
        isolation.token("one-too-many");
        assert_eq!(isolation.0.lock().unwrap().len(), 1);
        assert_ne!(isolation.token("site0"), first);
    }
}

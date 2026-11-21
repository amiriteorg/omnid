use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arti_client::config::{BoolOrAuto, TorClientConfigBuilder};
use arti_client::{BootstrapBehavior, DataStream, StreamPrefs, TorClient};
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

pub(crate) struct TorTransport {
    client: Arc<TorClient<PreferredRuntime>>,
    prefs: StreamPrefs,
    tls: TlsConnector,
    user_agent: String,
    pool: Pool,
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
        Ok(Self { client, prefs, tls: tls_connector()?, user_agent: user_agent.to_owned(), pool: Pool::default() })
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
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

use std::path::Path;
use std::sync::Arc;

use arti_client::config::{BoolOrAuto, TorClientConfigBuilder};
use arti_client::{BootstrapBehavior, StreamPrefs, TorClient};
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;
use tor_rtcompat::PreferredRuntime;
use url::Url;

use crate::http::{self, RawResponse};
use crate::{BoxError, FetchOptions, RouterState, RouterStatus};

/// Tor through arti, running inside the daemon: no separate Tor process.
pub(crate) struct TorTransport {
    client: Arc<TorClient<PreferredRuntime>>,
    prefs: StreamPrefs,
    tls: TlsConnector,
    user_agent: String,
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
        Ok(Self { client, prefs, tls: tls_connector()?, user_agent: user_agent.to_owned() })
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

    pub(crate) async fn get(&self, url: &Url, opts: &FetchOptions) -> Result<RawResponse, BoxError> {
        let host = url.host_str().ok_or("URL has no host")?;
        let port = url.port_or_known_default().ok_or("URL has no port")?;
        let stream = self.client.connect_with_prefs((host, port), &self.prefs).await?;
        if url.scheme() == "https" {
            let name = ServerName::try_from(host.to_owned())?;
            let stream = self.tls.connect(name, stream).await?;
            http::http1_get(stream, url, &self.user_agent, opts).await
        } else {
            http::http1_get(stream, url, &self.user_agent, opts).await
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

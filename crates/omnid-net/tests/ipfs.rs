//! IPFS through gateways, against local stand-ins: what is asked of them,
//! redirects inside the content, failover, and refusing a server that is not
//! one.

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::IntoResponse;
use omnid_core::{Network, Source};
use omnid_net::{FetchError, FetchOptions, Gateway, GatewayConfig, RouterState};
use tokio::net::TcpListener;
use url::Url;

const CID: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";

/// A path gateway, as kubo 0.43's on 127.0.0.1: every answer names its
/// content in `Ipfs-Uri` and `X-Ipfs-Roots`, a directory without its slash
/// redirects to one with it,
/// and `/moved` redirects to the gateway's own absolute URL. Records the
/// paths asked for.
async fn stand_in_gateway() -> (Url, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |uri: Uri| {
        log.lock().unwrap().push(uri.to_string());
        let origin = origin.clone();
        async move {
            let path = uri.path().to_owned();
            let mut headers = HeaderMap::new();
            // /ipfs/CID/x is ipfs://CID/x.
            let uri = format!("{}:/{}", path.get(1..5).unwrap_or("ipfs"), path.get(5..).unwrap_or_default());
            headers.insert("ipfs-uri", uri.parse().unwrap());
            headers.insert("x-ipfs-roots", CID.parse().unwrap());
            if path.ends_with("/dir") {
                headers.insert(header::LOCATION, format!("{path}/").parse().unwrap());
                return (StatusCode::MOVED_PERMANENTLY, headers, String::new()).into_response();
            }
            if path.ends_with("/moved") {
                headers.insert(header::LOCATION, format!("{origin}/ipfs/{CID}/here/").parse().unwrap());
                return (StatusCode::FOUND, headers, String::new()).into_response();
            }
            headers.insert(header::CONTENT_TYPE, "text/html".parse().unwrap());
            (StatusCode::OK, headers, format!("<title>{path}</title>")).into_response()
        }
    });
    let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, asked)
}

/// Something else on the gateway's port: a web server that answers every
/// path, without a gateway's headers.
async fn not_a_gateway() -> Url {
    let app =
        axum::Router::new().fallback(|| async { ([(header::CONTENT_TYPE, "text/html")], "<title>dev server</title>") });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn gateway(ipfs_gateways: Vec<Url>, clearnet_via_tor: bool) -> Gateway {
    Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        ipfs_gateways,
        clearnet_via_tor,
        ..GatewayConfig::default()
    })
    .unwrap()
}

fn opts() -> FetchOptions {
    FetchOptions { accept: |_| true, ..FetchOptions::default() }
}

#[tokio::test]
async fn asks_the_gateway_for_the_canonical_path_and_follows_its_redirects() {
    let (url, asked) = stand_in_gateway().await;
    let gateway = gateway(vec![url.clone()], false);
    // A CIDv0 is asked for as CIDv1, the form every gateway takes.
    let source = Source::parse("ipfs://QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn/docs/dir").unwrap();
    let page = gateway.fetch(&source, &opts()).await.unwrap();
    assert_eq!(page.status, 200);
    assert_eq!(page.source.to_string(), format!("ipfs://{CID}/docs/dir/"), "the slash redirect stays in IPFS");
    assert_eq!(*asked.lock().unwrap(), [format!("/ipfs/{CID}/docs/dir"), format!("/ipfs/{CID}/docs/dir/")]);

    // A redirect to the gateway's own absolute URL is a path in IPFS, not a
    // loopback page on the clearnet.
    let page = gateway.fetch(&Source::parse(&format!("ipfs://{CID}/moved")).unwrap(), &opts()).await.unwrap();
    assert_eq!(page.source.to_string(), format!("ipfs://{CID}/here/"));
    assert_eq!(page.source.network(), Network::Ipfs);

    let page = gateway.fetch(&Source::parse("ipns://docs.ipfs.tech/install/").unwrap(), &opts()).await.unwrap();
    assert_eq!(page.body.as_deref(), Some(&b"<title>/ipns/docs.ipfs.tech/install/</title>"[..]));
    assert_eq!(gateway.ipfs_gateway(), Some(&url));
}

#[tokio::test]
async fn passes_over_a_server_that_is_not_a_gateway() {
    let impostor = not_a_gateway().await;
    let (real, _) = stand_in_gateway().await;
    let both = gateway(vec![impostor.clone(), real], false);
    let source = Source::parse(&format!("ipfs://{CID}/")).unwrap();
    let page = both.fetch(&source, &opts()).await.unwrap();
    assert_eq!(page.body.as_deref(), Some(format!("<title>/ipfs/{CID}/</title>").as_bytes()));

    let alone = gateway(vec![impostor], false);
    match alone.fetch(&source, &opts()).await {
        Err(FetchError::Unavailable { network: Network::Ipfs, reason }) => {
            assert!(reason.contains("without an IPFS gateway's headers"), "{reason}")
        }
        other => panic!("took a page from a server that is no gateway: {other:?}"),
    }
}

#[tokio::test]
async fn reports_each_gateway_and_none_configured() {
    let (url, _) = stand_in_gateway().await;
    let status = gateway(vec!["http://127.0.0.1:9/".parse().unwrap(), url.clone()], false).status().await;
    let ipfs = status.iter().find(|s| s.network == Network::Ipfs).unwrap();
    assert_eq!(ipfs.state, RouterState::Ready);
    assert_eq!(ipfs.detail, format!("no IPFS gateway at http://127.0.0.1:9/; IPFS gateway at {url}"));

    let none = gateway(vec![], false);
    let ipfs = none.status().await.into_iter().find(|s| s.network == Network::Ipfs).unwrap();
    assert_eq!(ipfs.state, RouterState::Disabled);
    let error = none.fetch(&Source::parse(&format!("ipfs://{CID}/")).unwrap(), &opts()).await.unwrap_err();
    assert!(error.to_string().contains("no IPFS gateway configured"), "{error}");
}

#[tokio::test]
async fn the_browser_and_proxy_never_reach_the_gateway() {
    // IPFS fetches come from the node's own address: a page in the browser
    // must not make it fetch a CID of the page's choosing.
    let (url, asked) = stand_in_gateway().await;
    let gateway = gateway(vec![url], false);
    assert!(gateway.connect_browser("ipfs", 80, "ipfs").await.is_err());
    assert!(gateway.connect_browser(&format!("{CID}.ipfs.localhost"), 80, "x").await.is_err());
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn ipfs_is_reached_directly_so_anonymous_pages_do_not_lead_there() {
    let direct = gateway(vec![], false);
    assert!(direct.leaves_anonymity(Network::Onion, Network::Ipfs));
    assert!(direct.leaves_anonymity(Network::I2p, Network::Ipfs));
    assert!(direct.leaves_anonymity(Network::Hyphanet, Network::Ipfs));
    assert!(!direct.leaves_anonymity(Network::Clearnet, Network::Ipfs));
    assert!(!direct.leaves_anonymity(Network::Ipfs, Network::Clearnet));
    assert!(!direct.leaves_anonymity(Network::Ipfs, Network::Onion));
    // What held before IPFS still does.
    assert!(direct.leaves_anonymity(Network::Onion, Network::Clearnet));
    assert!(!direct.leaves_anonymity(Network::Clearnet, Network::Onion));

    let via_tor = gateway(vec![], true);
    assert!(via_tor.leaves_anonymity(Network::Clearnet, Network::Ipfs), "a page read through Tor");
    assert!(!via_tor.leaves_anonymity(Network::Onion, Network::Clearnet));
}

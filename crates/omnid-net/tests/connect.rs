//! Raw streams from `Gateway::connect`, routed by name, against local
//! stand-ins: an echo server (also standing in for a Hyphanet node) and a
//! fake i2pd CONNECT proxy.

use omnid_net::{FetchError, Gateway, GatewayConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Echoes every connection back to itself.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

/// Answers CONNECT for `.i2p` targets by tunnelling to the echo server.
async fn fake_i2pd(echo: u16) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("CONNECT stats.i2p:80 "), "{line}");
                while line != "\r\n" {
                    line.clear();
                    stream.read_line(&mut line).await.unwrap();
                }
                let mut stream = stream.into_inner();
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", echo)).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
            });
        }
    });
    port
}

fn gateway(cfg: GatewayConfig) -> Gateway {
    Gateway::new(GatewayConfig { tor_dir: None, fproxies: vec![], ..cfg }).unwrap()
}

async fn round_trip(stream: &mut omnid_net::BoxStream) -> String {
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await.unwrap();
    String::from_utf8_lossy(&reply).into_owned()
}

#[tokio::test]
async fn connects_directly_when_private_addresses_are_allowed() {
    let echo = echo_server().await;
    let gateway = gateway(GatewayConfig { allow_private_addresses: true, ..GatewayConfig::default() });
    let mut stream = gateway.connect("127.0.0.1", echo).await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");
    let mut stream = gateway.connect("localhost", echo).await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");
}

#[tokio::test]
async fn refuses_private_addresses_by_default() {
    let echo = echo_server().await;
    let gateway = gateway(GatewayConfig::default());
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let error = gateway.connect(host, echo).await.err().expect("refused");
        assert!(matches!(error, FetchError::PrivateAddress(_)), "{host}: {error}");
    }
}

#[tokio::test]
async fn onions_need_tor() {
    let gateway = gateway(GatewayConfig::default());
    let onion = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    let error = gateway.connect(onion, 80).await.err().expect("refused");
    assert!(matches!(error, FetchError::Unavailable { .. }), "{error}");
}

#[tokio::test]
async fn tunnels_i2p_through_the_router_proxy() {
    let echo = echo_server().await;
    let i2pd = fake_i2pd(echo).await;
    let tunnelled = gateway(GatewayConfig {
        i2p_proxies: vec![format!("http://127.0.0.1:{i2pd}").parse().unwrap()],
        ..GatewayConfig::default()
    });
    let mut stream = tunnelled.connect("Stats.I2P.", 80).await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");

    let no_router =
        gateway(GatewayConfig { i2p_proxies: vec!["http://127.0.0.1:9".parse().unwrap()], ..GatewayConfig::default() });
    let error = no_router.connect("stats.i2p", 80).await.err().expect("refused");
    assert!(error.to_string().contains("is the router running"), "{error}");
}

/// Counts the connections a listener accepts.
async fn counting_server() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    (port, accepted)
}

#[tokio::test]
async fn the_browser_never_connects_directly() {
    // Everything a direct connection would need is allowed, and clearnet is
    // not sent through Tor for the crawler; Tor itself is off.
    let (port, accepted) = counting_server().await;
    let gateway = gateway(GatewayConfig { allow_private_addresses: true, ..GatewayConfig::default() });
    for host in ["127.0.0.1", "localhost", "example.com", "x.onion"] {
        let error = gateway.connect_browser(host, port, "example.com").await.err().expect("refused");
        assert!(matches!(error, FetchError::Unavailable { .. }), "{host}: {error}");
    }
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 0, "no connection reached the server");
}

#[tokio::test]
async fn the_browser_reaches_i2p_through_the_router() {
    let echo = echo_server().await;
    let i2pd = fake_i2pd(echo).await;
    let gateway = gateway(GatewayConfig {
        i2p_proxies: vec![format!("http://127.0.0.1:{i2pd}").parse().unwrap()],
        ..GatewayConfig::default()
    });
    let mut stream = gateway.connect_browser("stats.i2p", 80, "stats.i2p").await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");
}

fn with_node(port: u16) -> Gateway {
    Gateway::new(GatewayConfig {
        tor_dir: None,
        fproxies: vec![format!("http://127.0.0.1:{port}/").parse().unwrap()],
        ..GatewayConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn hyphanet_pages_reach_the_node_and_nothing_else() {
    // `hyphanet` names the node's FProxy, for the browser and the SOCKS proxy
    // alike; the stand-in node echoes.
    let node = echo_server().await;
    let gateway = with_node(node);
    let mut stream = gateway.connect_browser("hyphanet", 80, "hyphanet").await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");
    let mut stream = gateway.connect("Hyphanet.", 80).await.unwrap();
    assert_eq!(round_trip(&mut stream).await, "ping");
    // Only port 80: an HTTPS attempt is refused at once.
    let error = gateway.connect_browser("hyphanet", 443, "hyphanet").await.err().expect("refused");
    assert!(matches!(error, FetchError::Transport { .. }), "{error}");
    // The node's own address is not a way around the rule.
    let error = gateway.connect_browser("127.0.0.1", node, "127.0.0.1").await.err().expect("refused");
    assert!(matches!(error, FetchError::Unavailable { .. }), "{error}");
}

#[tokio::test]
async fn hyphanet_pages_need_a_node() {
    let error = gateway(GatewayConfig::default()).connect_browser("hyphanet", 80, "hyphanet").await.err();
    assert!(matches!(error, Some(FetchError::Unavailable { .. })), "{error:?}");
    // A node that is not running says so.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let error = with_node(closed).connect_browser("hyphanet", 80, "hyphanet").await.err().expect("refused");
    assert!(error.to_string().contains("is the node running"), "{error}");
}

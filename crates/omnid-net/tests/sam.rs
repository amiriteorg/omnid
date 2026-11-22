//! I2P identities through SAM, against a local stand-in for a router's SAM
//! bridge: each site the browser is on gets a session of its own, a new
//! identity closes them, and without a bridge streams go through the HTTP
//! proxy as before.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omnid_core::Network;
use omnid_net::{BoxStream, Gateway, GatewayConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

/// What the stand-in bridge saw.
#[derive(Default)]
struct Seen {
    /// Each SESSION CREATE's ID and options, in order.
    created: Vec<(String, String)>,
    /// Sessions whose control connection is open.
    open: HashSet<String>,
    /// Sessions the bridge answers INVALID_ID for, as a restarted router does.
    forgotten: HashSet<String>,
    /// PONGs to the bridge's PING.
    pongs: usize,
    /// STREAM CONNECTs, by target.
    connects: Vec<String>,
}

struct Bridge {
    url: Url,
    seen: Arc<Mutex<Seen>>,
}

impl Bridge {
    fn seen<T>(&self, f: impl FnOnce(&mut Seen) -> T) -> T {
        f(&mut self.seen.lock().unwrap())
    }
}

/// A SAM bridge as i2pd's answers: sessions are made at once; a stream
/// answers with the ID of the session it came from, then its target.
async fn bridge() -> Bridge {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("sam://{}", listener.local_addr().unwrap()).parse().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let shared = seen.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve(socket, shared.clone()));
        }
    });
    Bridge { url, seen }
}

async fn serve(socket: TcpStream, seen: Arc<Mutex<Seen>>) {
    let mut socket = BufReader::new(socket);
    let mut line = String::new();
    if socket.read_line(&mut line).await.unwrap_or(0) == 0 || !line.starts_with("HELLO VERSION") {
        return;
    }
    socket.get_mut().write_all(b"HELLO REPLY RESULT=OK VERSION=3.3\n").await.unwrap();
    line.clear();
    if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let id = words.iter().find_map(|w| w.strip_prefix("ID=")).unwrap_or_default().to_owned();
    match words.get(..2) {
        Some(["SESSION", "CREATE"]) => {
            seen.lock().unwrap().created.push((id.clone(), line.clone()));
            seen.lock().unwrap().open.insert(id.clone());
            let reply = format!("SESSION STATUS RESULT=OK DESTINATION={}\nPING probe\n", "K".repeat(900));
            socket.get_mut().write_all(reply.as_bytes()).await.unwrap();
            line.clear();
            while socket.read_line(&mut line).await.unwrap_or(0) > 0 {
                match line.trim_end() {
                    "PONG probe" => seen.lock().unwrap().pongs += 1,
                    // As i2pd 3.0 does.
                    "PING" => break,
                    _ => {}
                }
                line.clear();
            }
            seen.lock().unwrap().open.remove(&id);
        }
        Some(["STREAM", "CONNECT"]) => {
            let host = words.iter().find_map(|w| w.strip_prefix("DESTINATION=")).unwrap_or_default();
            let port = words.iter().find_map(|w| w.strip_prefix("TO_PORT=")).unwrap_or_default();
            let known = {
                let mut seen = seen.lock().unwrap();
                seen.connects.push(host.to_owned());
                seen.open.contains(&id) && !seen.forgotten.contains(&id)
            };
            let reply = match host {
                _ if !known => "STREAM STATUS RESULT=INVALID_ID\n".to_owned(),
                "nobody.i2p" => "STREAM STATUS RESULT=INVALID_KEY\n".to_owned(),
                "gone.i2p" => "STREAM STATUS RESULT=CANT_REACH_PEER MESSAGE=\"no leaseset\"\n".to_owned(),
                _ => format!("STREAM STATUS RESULT=OK\n{id} {host}:{port}\n"),
            };
            socket.get_mut().write_all(reply.as_bytes()).await.unwrap();
            let _ = socket.read(&mut [0; 1]).await;
        }
        _ => {}
    }
}

/// An i2pd HTTP proxy that opens every CONNECT tunnel and says it did.
async fn http_proxy() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = socket.read_u8().await else { return };
                    head.push(byte);
                }
                socket.write_all(b"HTTP/1.1 200 OK\r\n\r\nthe proxy's destination\n").await.unwrap();
                let _ = socket.read(&mut [0; 1]).await;
            });
        }
    });
    url
}

fn gateway(sam: Vec<Url>, proxies: Vec<Url>) -> Gateway {
    Gateway::new(GatewayConfig { tor_dir: None, i2p_proxies: proxies, i2p_sam: sam, ..GatewayConfig::default() })
        .unwrap()
}

/// The first line a stream gets.
async fn first_line(stream: &mut BoxStream) -> String {
    let mut line = Vec::new();
    while !line.ends_with(b"\n") {
        line.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(line).unwrap().trim_end().to_owned()
}

/// The session a stream for `host`, from a page of `site`, came from.
async fn identity(gateway: &Gateway, host: &str, site: &str) -> String {
    let mut stream = gateway.connect_browser(host, 80, site).await.unwrap();
    let line = first_line(&mut stream).await;
    assert!(line.ends_with(&format!(" {host}:80")), "{line}");
    line.split(' ').next().unwrap().to_owned()
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

#[tokio::test]
async fn each_site_gets_an_i2p_identity_of_its_own() {
    let bridge = bridge().await;
    let gateway = gateway(vec![bridge.url.clone()], vec![]);
    let a = identity(&gateway, "a.i2p", "a.i2p").await;
    assert_eq!(identity(&gateway, "a.i2p", "a.i2p").await, a, "a site keeps its identity");
    assert_eq!(identity(&gateway, "b.i2p", "a.i2p").await, a, "what a page loads comes from its site's identity");
    until("two spares are ready besides the site's", || bridge.seen(|s| s.open.len() == 3)).await;
    let made: Vec<String> = bridge.seen(|s| s.created.iter().map(|(id, _)| id.clone()).collect());
    let b = identity(&gateway, "b.i2p", "b.i2p").await;
    assert!(made.contains(&b), "a new site takes a spare made before it came");
    let app = identity(&gateway, "a.i2p", "app:feeds").await;
    assert!(a != b && app != a && app != b, "{a} {b} {app}");

    let once = identity(&gateway, "a.i2p", "~4f1e").await;
    let again = identity(&gateway, "a.i2p", "~4f1e").await;
    assert!(once != again && ![&a, &b, &app].contains(&&once), "a one-off key is used once");
    until("the one-off sessions are closed", || bridge.seen(|s| !s.open.contains(&once) && !s.open.contains(&again)))
        .await;
    assert!(bridge.seen(|s| s.open.contains(&a) && s.open.contains(&b)), "the sites' are kept");

    bridge.seen(|s| {
        for (_, command) in &s.created {
            assert!(command.contains(" DESTINATION=TRANSIENT "), "{command}");
            assert!(
                command.contains(" inbound.length=3 outbound.length=3 "),
                "3 hops, whatever the router's default: {command}"
            );
            assert!(command.contains(" i2cp.dontPublishLeaseSet=true"), "{command}");
        }
    });
    until("the bridge's PING is answered", || bridge.seen(|s| s.pongs > 0)).await;
}

#[tokio::test]
async fn a_new_identity_closes_every_sites_session() {
    let bridge = bridge().await;
    let gateway = gateway(vec![bridge.url.clone()], vec![]);
    let a = identity(&gateway, "a.i2p", "a.i2p").await;
    let b = identity(&gateway, "b.i2p", "b.i2p").await;
    gateway.new_identity();
    until("the sites' sessions are closed", || bridge.seen(|s| !s.open.contains(&a) && !s.open.contains(&b))).await;
    let again = identity(&gateway, "a.i2p", "a.i2p").await;
    assert!(again != a && again != b);
}

#[tokio::test]
async fn a_session_the_router_forgot_is_made_again() {
    let bridge = bridge().await;
    let gateway = gateway(vec![bridge.url.clone()], vec![]);
    let a = identity(&gateway, "a.i2p", "a.i2p").await;
    bridge.seen(|s| s.forgotten.insert(a.clone()));
    let again = identity(&gateway, "a.i2p", "a.i2p").await;
    assert_ne!(again, a);
    until("the forgotten session is closed", || bridge.seen(|s| !s.open.contains(&a))).await;
}

#[tokio::test]
async fn keeps_at_most_twelve_sites_sessions() {
    let bridge = bridge().await;
    let gateway = gateway(vec![bridge.url.clone()], vec![]);
    let first = identity(&gateway, "a.i2p", "site0.i2p").await;
    for i in 1..=12 {
        identity(&gateway, "a.i2p", &format!("site{i}.i2p")).await;
    }
    until("the least recently used is closed", || bridge.seen(|s| !s.open.contains(&first))).await;
    assert_ne!(identity(&gateway, "a.i2p", "site0.i2p").await, first);
}

#[tokio::test]
async fn sam_says_why_a_stream_failed() {
    let bridge = bridge().await;
    let gateway = gateway(vec![bridge.url.clone()], vec![http_proxy().await]);
    let unknown = gateway.connect_browser("nobody.i2p", 80, "nobody.i2p").await.err().unwrap();
    assert!(unknown.to_string().contains("address book has no nobody.i2p"), "{unknown}");
    let gone = gateway.connect_browser("gone.i2p", 80, "gone.i2p").await.err().unwrap();
    assert!(gone.to_string().contains("gone.i2p did not answer over I2P: no leaseset"), "{gone}");
    let tries = |host: &str| bridge.seen(|s| s.connects.iter().filter(|h| *h == host).count());
    assert_eq!((tries("gone.i2p"), tries("nobody.i2p")), (2, 1), "a site not found is looked up twice; a name, once");
    for bad in ["a b.i2p", "a\nSESSION CREATE.i2p"] {
        let error = gateway.connect_browser(bad, 80, "a.i2p").await.err().unwrap();
        assert!(error.to_string().contains("not an I2P host name"), "{error}");
    }
    // None of these went to the proxy, on the one destination every site shares.
}

#[tokio::test]
async fn without_a_bridge_streams_go_through_the_http_proxy() {
    let down: Url = "sam://127.0.0.1:9".parse().unwrap();
    let gateway = gateway(vec![down], vec![http_proxy().await]);
    let mut stream = gateway.connect_browser("a.i2p", 80, "a.i2p").await.unwrap();
    assert_eq!(first_line(&mut stream).await, "the proxy's destination");
    let status = gateway.status().await;
    let i2p = status.iter().find(|s| s.network == Network::I2p).unwrap();
    assert!(i2p.detail.ends_with("; no SAM at sam://127.0.0.1:9; one I2P identity for every site"), "{}", i2p.detail);

    let bridge = bridge().await;
    let both = gateway_with(&bridge).await;
    let status = both.status().await;
    let i2p = status.iter().find(|s| s.network == Network::I2p).unwrap();
    assert!(i2p.detail.ends_with(&format!("; SAM at {}; an I2P identity per site", bridge.url)), "{}", i2p.detail);
    until("the daemon's look at the status made spares", || bridge.seen(|s| s.open.len() == 2)).await;
    // The crawler's streams, and the open proxy's, stay on the HTTP proxy.
    let mut shared = both.connect("a.i2p", 80).await.unwrap();
    assert_eq!(first_line(&mut shared).await, "the proxy's destination");
}

async fn gateway_with(bridge: &Bridge) -> Gateway {
    gateway(vec![bridge.url.clone()], vec![http_proxy().await])
}

#[tokio::test]
async fn a_bridge_that_never_ends_its_line_is_not_followed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("sam://{}", listener.local_addr().unwrap()).parse().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let chunk = [b'A'; 4096];
                while socket.write_all(&chunk).await.is_ok() {}
            });
        }
    });
    let gateway = gateway(vec![url], vec![http_proxy().await]);
    let mut stream = tokio::time::timeout(Duration::from_secs(20), gateway.connect_browser("a.i2p", 80, "a.i2p"))
        .await
        .expect("the endless line is cut off")
        .unwrap();
    assert_eq!(first_line(&mut stream).await, "the proxy's destination", "a bridge that is not SAM is passed over");
}

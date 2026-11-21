//! The UDP side: one socket, outgoing queries paced by the global rate
//! limit, and a receive loop that hands each reply to the query waiting on
//! its transaction ID. Anything else that arrives is dropped.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{oneshot, watch};

use crate::krpc::{self, Message, NodeId, NodeInfo, Query, Response};
use crate::routing::{K, RoutingTable};
use crate::{DhtConfig, DhtStatus};

/// How long a node gets to answer one query.
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub(crate) enum QueryError {
    #[error("no answer")]
    Timeout,
    #[error("node answered with error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

enum Reply {
    Response(Response),
    Error { code: i64, message: String },
}

struct Pending {
    addr: SocketAddrV4,
    reply: oneshot::Sender<Reply>,
}

/// State shared by the socket, the lookups and the harvester.
pub(crate) struct Core {
    pub(crate) cfg: DhtConfig,
    pub(crate) id: NodeId,
    /// The BitTorrent peer ID used for metadata connections.
    pub(crate) peer_id: [u8; 20],
    table: Mutex<RoutingTable>,
    /// Bootstrap routers answer find_node but are kept out of the table, so
    /// lookups do not lean on them.
    routers: Mutex<HashSet<SocketAddrV4>>,
    socket: UdpSocket,
    pending: Mutex<HashMap<[u8; 4], Pending>>,
    pacer: Pacer,
    ready: watch::Sender<bool>,
    queries_sent: AtomicU64,
    responses: AtomicU64,
    /// Lets addresses learned from the network be private. Tests only.
    allow_private: bool,
}

impl Core {
    pub(crate) async fn bind(cfg: DhtConfig, allow_private: bool) -> io::Result<Self> {
        if !cfg.bind.is_ipv4() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the DHT client is IPv4-only"));
        }
        let socket = UdpSocket::bind(cfg.bind).await?;
        let id = rand::random();
        Ok(Self {
            pacer: Pacer::new(cfg.max_queries_per_sec),
            cfg,
            id,
            peer_id: crate::peer::peer_id(),
            table: Mutex::new(RoutingTable::new(id)),
            routers: Mutex::default(),
            socket,
            pending: Mutex::default(),
            ready: watch::Sender::new(false),
            queries_sent: AtomicU64::new(0),
            responses: AtomicU64::new(0),
            allow_private,
        })
    }

    /// Sends one query and waits for its answer.
    pub(crate) async fn query(&self, addr: SocketAddrV4, query: &Query) -> Result<Response, QueryError> {
        self.pacer.wait().await;
        let (reply, answer) = oneshot::channel();
        let tid = {
            let mut pending = self.pending.lock().expect("dht pending lock poisoned");
            loop {
                let tid: [u8; 4] = rand::random();
                if let Entry::Vacant(slot) = pending.entry(tid) {
                    slot.insert(Pending { addr, reply });
                    break tid;
                }
            }
        };
        // Unregisters the query however this future ends, dropped included.
        let _registered = Registered { core: self, tid };
        self.socket.send_to(&krpc::encode_query(&tid, &self.id, query), addr).await?;
        self.queries_sent.fetch_add(1, Ordering::Relaxed);
        match tokio::time::timeout(QUERY_TIMEOUT, answer).await {
            Ok(Ok(Reply::Response(response))) => Ok(response),
            Ok(Ok(Reply::Error { code, message })) => Err(QueryError::Remote { code, message }),
            Ok(Err(_)) | Err(_) => Err(QueryError::Timeout),
        }
    }

    /// The receive loop. Runs until the task is aborted.
    pub(crate) async fn receive(self: std::sync::Arc<Self>) {
        let mut buf = vec![0; 65536];
        loop {
            let (len, from) = match self.socket.recv_from(&mut buf).await {
                Ok(received) => received,
                Err(e) => {
                    // Some platforms report ICMP errors for earlier sends here.
                    tracing::trace!("dht: receive failed: {e}");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            };
            let SocketAddr::V4(from) = from else {
                continue;
            };
            match krpc::decode(&buf[..len]) {
                Ok(Message::Response { tid, response }) => self.resolve(&tid, from, Reply::Response(response)),
                Ok(Message::Error { tid, code, message }) => self.resolve(&tid, from, Reply::Error { code, message }),
                Ok(Message::Query { method }) => {
                    tracing::trace!(%from, method = %String::from_utf8_lossy(&method), "dht: not answering a query")
                }
                Err(e) => tracing::trace!(%from, "dht: dropping a malformed packet: {e}"),
            }
        }
    }

    /// Hands a reply to the query that is waiting for it. Only the address
    /// that was asked can answer, so a spoofed reply has to guess both the
    /// address and a random 32-bit transaction ID.
    fn resolve(&self, tid: &[u8], from: SocketAddrV4, reply: Reply) {
        let Ok(tid) = <[u8; 4]>::try_from(tid) else {
            return;
        };
        let mut pending = self.pending.lock().expect("dht pending lock poisoned");
        if let Entry::Occupied(entry) = pending.entry(tid)
            && entry.get().addr == from
        {
            self.responses.fetch_add(1, Ordering::Relaxed);
            let _ = entry.remove().reply.send(reply);
        }
    }

    /// Records a node that answered. Routers and unusable addresses stay out.
    /// With `K` nodes in the table lookups can start, even while the
    /// bootstrap is still filling it.
    pub(crate) fn heard_from(&self, info: NodeInfo) {
        if self.routers.lock().expect("dht routers lock poisoned").contains(&info.addr) || !self.usable_node(info.addr)
        {
            return;
        }
        let nodes = {
            let mut table = self.table.lock().expect("dht table lock poisoned");
            table.heard_from(info, Instant::now());
            table.len()
        };
        if nodes >= K && !*self.ready.borrow() {
            self.set_ready(true);
        }
    }

    pub(crate) fn failed(&self, addr: SocketAddrV4) {
        self.table.lock().expect("dht table lock poisoned").failed(addr);
    }

    pub(crate) fn closest(&self, target: &NodeId, n: usize) -> Vec<NodeInfo> {
        self.table.lock().expect("dht table lock poisoned").closest(target, n)
    }

    pub(crate) fn stale(&self, before: Instant, n: usize) -> Vec<NodeInfo> {
        self.table.lock().expect("dht table lock poisoned").stale(before, n)
    }

    pub(crate) fn table_nodes(&self) -> Vec<NodeInfo> {
        self.table.lock().expect("dht table lock poisoned").nodes().collect()
    }

    pub(crate) fn table_len(&self) -> usize {
        self.table.lock().expect("dht table lock poisoned").len()
    }

    pub(crate) fn set_routers(&self, routers: &[SocketAddrV4]) {
        *self.routers.lock().expect("dht routers lock poisoned") = routers.iter().copied().collect();
    }

    pub(crate) fn set_ready(&self, ready: bool) {
        self.ready.send_replace(ready);
    }

    /// Waits up to `limit` for the first bootstrap to finish.
    pub(crate) async fn wait_ready(&self, limit: Duration) -> bool {
        let mut ready = self.ready.subscribe();
        tokio::time::timeout(limit, ready.wait_for(|ready| *ready)).await.is_ok_and(|r| r.is_ok())
    }

    /// Whether a node address learned from the network may be contacted.
    pub(crate) fn usable_node(&self, addr: SocketAddrV4) -> bool {
        addr.port() != 0 && (self.allow_private || is_public(*addr.ip()))
    }

    /// Whether a peer address learned from the network may be contacted.
    /// Ports below 1024 are skipped as well: poisoned DHT entries have been
    /// used to point BitTorrent clients at web servers.
    pub(crate) fn usable_peer(&self, addr: SocketAddrV4) -> bool {
        addr.port() >= 1024 && (self.allow_private || is_public(*addr.ip()))
    }

    pub(crate) fn status(&self) -> DhtStatus {
        let nodes = self.table_len();
        DhtStatus {
            nodes,
            ready: *self.ready.borrow() && nodes > 0,
            queries_sent: self.queries_sent.load(Ordering::Relaxed),
            responses: self.responses.load(Ordering::Relaxed),
        }
    }
}

struct Registered<'a> {
    core: &'a Core,
    tid: [u8; 4],
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.core.pending.lock().expect("dht pending lock poisoned").remove(&self.tid);
    }
}

/// Spaces outgoing queries evenly, at most `rate` a second. No bursts: the
/// DHT is shared infrastructure.
struct Pacer {
    gap: Duration,
    next: Mutex<Instant>,
}

impl Pacer {
    fn new(rate: u32) -> Self {
        Self { gap: Duration::from_secs(1) / rate.max(1), next: Mutex::new(Instant::now()) }
    }

    async fn wait(&self) {
        let slot = {
            let mut next = self.next.lock().expect("dht pacer lock poisoned");
            let slot = (*next).max(Instant::now());
            *next = slot + self.gap;
            slot
        };
        tokio::time::sleep_until(slot.into()).await;
    }
}

/// Whether an address is public unicast IPv4. Addresses learned from the
/// network are only contacted if they are, so a hostile node cannot point
/// us at the operator's LAN.
pub(crate) fn is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_unicast_is_public() {
        for ip in ["127.0.0.1", "10.0.0.1", "192.168.1.1", "172.16.5.5", "169.254.1.1", "100.64.0.1", "0.1.2.3"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["224.0.0.1", "255.255.255.255", "240.1.1.1", "192.0.2.1", "198.18.0.1", "192.0.0.8"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "82.221.103.244", "67.215.246.10"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn pacer_spaces_queries() {
        let pacer = Pacer::new(10);
        let start = Instant::now();
        for _ in 0..5 {
            pacer.wait().await;
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    }

    #[tokio::test]
    async fn queries_time_out_and_unregister() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = silent.local_addr().unwrap() else { unreachable!() };
        let cfg = DhtConfig { bind: "127.0.0.1:0".parse().unwrap(), ..DhtConfig::default() };
        let core = Core::bind(cfg, true).await.unwrap();
        let result = core.query(addr, &Query::Ping).await;
        assert!(matches!(result, Err(QueryError::Timeout)), "{result:?}");
        assert!(core.pending.lock().unwrap().is_empty());
        assert_eq!(core.status().queries_sent, 1);
    }

    #[tokio::test]
    async fn ignores_replies_from_the_wrong_address() {
        let node = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let impostor = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = node.local_addr().unwrap() else { unreachable!() };
        let cfg = DhtConfig { bind: "127.0.0.1:0".parse().unwrap(), ..DhtConfig::default() };
        let core = std::sync::Arc::new(Core::bind(cfg, true).await.unwrap());
        let client = core.socket.local_addr().unwrap();
        let receiver = tokio::spawn(core.clone().receive());
        tokio::spawn(async move {
            let mut buf = [0; 1500];
            let (len, from) = node.recv_from(&mut buf).await.unwrap();
            let query = krpc::testing::decode_query(&buf[..len]).unwrap();
            let response = Response { id: [7; 20], ..Default::default() };
            let packet = krpc::testing::encode_response(&query.tid, &response);
            // The impostor answers first, with the right transaction ID.
            impostor.send_to(&packet, client).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            node.send_to(&packet, from).await.unwrap();
        });
        let response = core.query(addr, &Query::Ping).await.unwrap();
        assert_eq!(response.id, [7; 20]);
        assert_eq!(core.status().responses, 1);
        receiver.abort();
    }
}

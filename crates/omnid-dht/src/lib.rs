//! A read-only BitTorrent Mainline DHT client, for two jobs:
//!
//! - [`Dht::metadata`] resolves a magnet's info hash to the torrent's name,
//!   files and sizes, and estimates the swarm, without downloading content.
//!   A `get_peers` lookup (BEP 5) finds peers; each is asked for the info
//!   dict over `ut_metadata` (BEP 9) as soon as it is found, while the lookup
//!   goes on, and the result is checked against the info hash.
//! - [`Dht::harvest`] yields info hashes sampled from DHT nodes with
//!   `sample_infohashes` (BEP 51), asking each node no more often than it
//!   says it wants to be asked.
//!
//! The node is read-only (BEP 43): every query says so, it answers no
//! queries, and it never announces, so it takes no place in anyone's routing
//! table and serves nothing. On the peer wire it never sends `interested` or
//! `request`, so no piece data is ever downloaded. Outgoing queries share
//! one global rate limit. Addresses learned from the network are contacted
//! only if they are public IPv4 addresses, so a hostile node cannot point
//! the daemon at the operator's LAN. IPv4 only.

mod bencode;
mod harvest;
mod krpc;
mod lookup;
mod meta;
mod peer;
mod routing;
mod rpc;

use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use omnid_core::FileMeta;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::krpc::NodeInfo;
use crate::lookup::Kind;
use crate::peer::PeerError;
use crate::routing::K;
use crate::rpc::Core;

/// The longest a get_peers lookup runs.
const LOOKUP_TIME: Duration = Duration::from_secs(30);
/// How long a metadata lookup goes on counting peers once the metadata is
/// in, unless it finishes sooner. See [`TorrentMeta::peers`].
const COUNT_TIME: Duration = Duration::from_secs(2);
/// How long a lookup waits for the DHT to become ready.
const READY_WAIT: Duration = Duration::from_secs(15);
/// The longest one peer gets to deliver the metadata.
const PEER_TIME: Duration = Duration::from_secs(30);
const HARVEST_BUFFER: usize = 1024;

#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// The UDP socket. Must be IPv4.
    pub bind: SocketAddr,
    /// `host:port` of the nodes that introduce us to the DHT.
    pub bootstrap: Vec<String>,
    /// Outgoing KRPC queries per second, across lookups and harvesting.
    pub max_queries_per_sec: u32,
    /// Bounds a whole [`Dht::metadata`] call: lookup and peer exchanges.
    pub metadata_timeout: Duration,
    /// Info dicts larger than this are refused before they are downloaded.
    pub max_metadata_bytes: usize,
    /// Peers asked for the metadata at once, per [`Dht::metadata`] call.
    pub max_peer_connections: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            bootstrap: [
                "router.bittorrent.com:6881",
                "dht.transmissionbt.com:6881",
                "router.utorrent.com:6881",
                "dht.libtorrent.org:25401",
            ]
            .map(String::from)
            .to_vec(),
            max_queries_per_sec: 50,
            metadata_timeout: Duration::from_secs(60),
            max_metadata_bytes: 10 << 20,
            max_peer_connections: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DhtStatus {
    /// Nodes in the routing table: ones that have answered us.
    pub nodes: usize,
    /// Lookups can start: enough nodes have answered, or the bootstrap
    /// finished with at least one.
    pub ready: bool,
    pub queries_sent: u64,
    pub responses: u64,
}

/// What an info dict says, plus the swarm estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentMeta {
    pub name: String,
    /// Single-file torrents list one file named after the torrent.
    /// Multi-file paths are relative to the torrent's directory, `name`,
    /// joined with `/`. BEP 47 padding files are left out.
    pub files: Vec<FileMeta>,
    pub total_size: u64,
    pub piece_length: u64,
    pub private: bool,
    /// Peers found by the lookup: a lower bound on the swarm's size. The
    /// lookup counts until it finishes, or for two seconds after the
    /// metadata arrived if it has not finished by then.
    pub peers: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    #[error("the DHT is not ready: no node has answered the bootstrap")]
    NotReady,
    #[error("the DHT knows no peers for this info hash")]
    NoPeers,
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The info dict matched the info hash but is not a valid torrent.
    #[error("bad metadata: {0}")]
    BadMetadata(String),
    #[error("metadata is {0} bytes, over the configured limit")]
    TooLarge(usize),
    #[error("none of the {tried} peers tried served the metadata; last error: {last}")]
    PeersFailed { tried: usize, last: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reads a torrent's info dictionary: the bytes whose SHA-1 is the info hash,
/// as [`Dht::metadata`] fetches them or a `.torrent` file holds them under
/// `info`. Hostile input is bounded; see `bencode::Limits::INFO`.
pub fn parse_info_dict(info: &[u8]) -> Result<TorrentMeta, DhtError> {
    meta::parse_info(info).map_err(DhtError::BadMetadata)
}

/// A handle on the DHT client. Clones share one socket and routing table;
/// the background tasks stop when the last clone is dropped.
#[derive(Clone)]
pub struct Dht {
    inner: Arc<Inner>,
}

struct Inner {
    core: Arc<Core>,
    tasks: Vec<AbortHandle>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Dht {
    /// Binds the UDP socket and bootstraps in the background. Call inside a
    /// Tokio runtime.
    pub async fn start(cfg: DhtConfig) -> io::Result<Dht> {
        Self::start_with(cfg, false).await
    }

    async fn start_with(cfg: DhtConfig, allow_private: bool) -> io::Result<Dht> {
        let core = Arc::new(Core::bind(cfg, allow_private).await?);
        let receive = tokio::spawn(core.clone().receive());
        let upkeep = tokio::spawn(lookup::upkeep(core.clone()));
        let tasks = vec![receive.abort_handle(), upkeep.abort_handle()];
        Ok(Dht { inner: Arc::new(Inner { core, tasks }) })
    }

    /// Iterative get_peers lookup; deduplicated peer addresses. An empty
    /// list means the DHT knows no peers.
    pub async fn get_peers(&self, info_hash: [u8; 20]) -> Result<Vec<SocketAddr>, DhtError> {
        let peers = self.lookup_peers(info_hash, LOOKUP_TIME).await?;
        Ok(peers.into_iter().map(SocketAddr::V4).collect())
    }

    /// get_peers and BEP 9 ut_metadata at once: each peer the lookup finds is
    /// asked right away, and the first info dict whose SHA-1 is info_hash
    /// wins.
    pub async fn metadata(&self, info_hash: [u8; 20]) -> Result<TorrentMeta, DhtError> {
        let limit = self.inner.core.cfg.metadata_timeout;
        let deadline = Instant::now() + limit;
        let seeds =
            tokio::time::timeout(limit, self.seeds(&info_hash)).await.map_err(|_| DhtError::Timeout(limit))??;
        self.fetch_metadata(info_hash, seeds, deadline).await
    }

    /// BEP 51: info hashes sampled from the DHT, honoring each node's
    /// `interval`. Ends when the receiver is dropped. Call inside a Tokio
    /// runtime.
    pub fn harvest(&self) -> mpsc::Receiver<[u8; 20]> {
        let (tx, rx) = mpsc::channel(HARVEST_BUFFER);
        // The task holds a handle, so the DHT runs while anyone harvests.
        let dht = self.clone();
        tokio::spawn(async move { harvest::run(&dht.inner.core, tx).await });
        rx
    }

    pub fn status(&self) -> DhtStatus {
        self.inner.core.status()
    }

    /// Where a lookup for `target` starts, once the DHT is ready.
    async fn seeds(&self, target: &[u8; 20]) -> Result<Vec<NodeInfo>, DhtError> {
        let core = &self.inner.core;
        if !core.wait_ready(READY_WAIT).await {
            return Err(DhtError::NotReady);
        }
        let seeds = core.closest(target, 2 * K);
        if seeds.is_empty() {
            return Err(DhtError::NotReady);
        }
        Ok(seeds)
    }

    async fn lookup_peers(&self, info_hash: [u8; 20], budget: Duration) -> Result<Vec<SocketAddrV4>, DhtError> {
        let seeds = self.seeds(&info_hash).await?;
        let deadline = Instant::now() + budget;
        let outcome = lookup::lookup(&self.inner.core, info_hash, Kind::GetPeers, seeds, deadline, None).await;
        log_lookup(&info_hash, &outcome);
        Ok(outcome.peers)
    }

    /// Runs the lookup as a task of its own and asks each peer it finds for
    /// the metadata as soon as it is found, up to `max_peer_connections` at
    /// once. The first verified info dict closes the other connections; the
    /// lookup then counts peers for up to [`COUNT_TIME`] more and is stopped.
    async fn fetch_metadata(
        &self,
        info_hash: [u8; 20],
        seeds: Vec<NodeInfo>,
        deadline: Instant,
    ) -> Result<TorrentMeta, DhtError> {
        let core = &self.inner.core;
        // The lookup gets at most half the time, as when it ran alone, so the
        // peers it finds last still have the other half.
        let lookup_end = (Instant::now() + (core.cfg.metadata_timeout / 2).min(LOOKUP_TIME)).min(deadline);
        // Unbounded, but a lookup sends at most MAX_PEERS. Dropping `search`
        // stops the lookup.
        let (found_tx, mut found) = mpsc::unbounded_channel();
        let mut search = JoinSet::new();
        let search_core = core.clone();
        search.spawn(async move {
            let outcome =
                lookup::lookup(&search_core, info_hash, Kind::GetPeers, seeds, lookup_end, Some(&found_tx)).await;
            log_lookup(&info_hash, &outcome);
        });

        let (mut peers, mut searching) = (0, true);
        let mut untried: Vec<SocketAddrV4> = Vec::new();
        let mut tasks = JoinSet::new();
        let (mut tried, mut too_large, mut last) = (0, None, String::from("no peer answered"));
        let (peer, info) = loop {
            while tasks.len() < core.cfg.max_peer_connections.max(1) && !untried.is_empty() {
                // At random, not in the order found, so a node that sends a
                // flood of peers does not get them all tried first.
                let peer = untried.swap_remove(rand::random_range(0..untried.len()));
                tried += 1;
                let (peer_id, max) = (core.peer_id, core.cfg.max_metadata_bytes);
                tasks.spawn(async move {
                    let fetch = peer::fetch_metadata(SocketAddr::V4(peer), &info_hash, &peer_id, max);
                    (peer, tokio::time::timeout(PEER_TIME, fetch).await.unwrap_or(Err(PeerError::Timeout)))
                });
            }
            // Until the lookup is over, a failure of every peer so far is not
            // the end: it may find more.
            if !searching && tasks.is_empty() {
                return Err(match too_large {
                    _ if peers == 0 => DhtError::NoPeers,
                    Some(size) => DhtError::TooLarge(size),
                    None => DhtError::PeersFailed { tried, last },
                });
            }
            tokio::select! {
                peer = found.recv(), if searching => match peer {
                    Some(peer) => {
                        peers += 1;
                        untried.push(peer);
                    }
                    None => searching = false,
                },
                Some(joined) = tasks.join_next(), if !tasks.is_empty() => match joined {
                    Ok((peer, Ok(info))) => break (peer, info),
                    Ok((peer, Err(e))) => {
                        tracing::trace!(%peer, "dht: no metadata from peer: {e}");
                        if let PeerError::TooLarge(size) = e {
                            too_large = Some(size);
                        }
                        last = e.to_string();
                    }
                    Err(e) => last = format!("peer task failed: {e}"),
                },
                _ = tokio::time::sleep_until(deadline.into()) => {
                    return Err(DhtError::Timeout(core.cfg.metadata_timeout));
                }
            }
        };
        // Dropping `tasks` closes the other connections.
        drop(tasks);
        // These bytes hash to the info hash, so they are the torrent's info
        // dict: no other peer can do better.
        let mut meta = meta::parse_info(&info).map_err(DhtError::BadMetadata)?;
        tracing::debug!(
            info_hash = %hex(&info_hash),
            %peer,
            name = %meta.name,
            files = meta.files.len(),
            tried,
            peers,
            "dht: got metadata"
        );
        let counted_by = (Instant::now() + COUNT_TIME).min(deadline);
        while searching {
            tokio::select! {
                peer = found.recv() => match peer {
                    Some(_) => peers += 1,
                    None => searching = false,
                },
                _ = tokio::time::sleep_until(counted_by.into()) => break,
            }
        }
        meta.peers = peers;
        Ok(meta)
    }
}

fn log_lookup(info_hash: &[u8; 20], outcome: &lookup::Outcome) {
    tracing::debug!(
        info_hash = %hex(info_hash),
        peers = outcome.peers.len(),
        queried = outcome.queried,
        answered = outcome.answered,
        "dht: get_peers lookup done"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::net::{TcpListener, UdpSocket};

    use super::*;
    use crate::bencode::Value;
    use crate::krpc::testing::{decode_query, encode_response};
    use crate::krpc::{NodeInfo, Query, Response};
    use crate::peer::testing::{Behavior, serve};

    #[derive(Default)]
    struct Seen {
        not_read_only: AtomicBool,
        samples_asked: AtomicUsize,
        get_peers_asked: AtomicUsize,
    }

    /// How a fake DHT node answers.
    #[derive(Clone, Default)]
    struct Node {
        id: [u8; 20],
        /// `nodes` in answers to everything but get_peers for `info_hash`.
        nodes: Vec<NodeInfo>,
        info_hash: [u8; 20],
        /// The answer to get_peers for `info_hash`, sent after `delay`.
        closer: Vec<NodeInfo>,
        peers: Vec<SocketAddrV4>,
        delay: Duration,
        /// The answer to sample_infohashes.
        samples: Vec<[u8; 20]>,
    }

    async fn fake_node(socket: UdpSocket, node: Node, seen: Arc<Seen>) {
        let socket = Arc::new(socket);
        let mut buf = [0; 2048];
        while let Ok((len, from)) = socket.recv_from(&mut buf).await {
            let Some(query) = decode_query(&buf[..len]) else {
                continue;
            };
            if !query.read_only {
                seen.not_read_only.store(true, Ordering::SeqCst);
            }
            let mut response = Response { id: node.id, nodes: node.nodes.clone(), ..Default::default() };
            let mut delay = Duration::ZERO;
            match query.query {
                Query::GetPeers { info_hash } if info_hash == node.info_hash => {
                    seen.get_peers_asked.fetch_add(1, Ordering::SeqCst);
                    (response.nodes, response.values, delay) = (node.closer.clone(), node.peers.clone(), node.delay);
                }
                Query::SampleInfohashes { .. } => {
                    seen.samples_asked.fetch_add(1, Ordering::SeqCst);
                    response.samples = node.samples.clone();
                    response.interval = Some(3600);
                    response.num = Some(node.samples.len() as u64);
                }
                _ => {}
            }
            let (socket, packet) = (socket.clone(), encode_response(&query.tid, &response));
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = socket.send_to(&packet, from).await;
            });
        }
    }

    /// What the fake peers saw.
    #[derive(Default)]
    struct PeerLog {
        /// IDs of the non-extension messages the client sent: must stay empty.
        forbidden: Mutex<Vec<u8>>,
        /// Connections open now, and the most ever open at once.
        open: AtomicUsize,
        most_open: AtomicUsize,
    }

    impl PeerLog {
        /// Waits a little for the client to close every connection.
        async fn all_closed(&self) -> bool {
            for _ in 0..40 {
                if self.open.load(Ordering::SeqCst) == 0 {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }
    }

    async fn fake_peer(info: &[u8], info_hash: [u8; 20], behavior: Behavior, log: Arc<PeerLog>) -> SocketAddrV4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = listener.local_addr().unwrap() else { unreachable!() };
        let info = info.to_vec();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (info, log) = (info.clone(), log.clone());
                log.most_open.fetch_max(log.open.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                tokio::spawn(async move {
                    if let Ok(ids) = serve(stream, info_hash, info, behavior).await {
                        log.forbidden.lock().unwrap().extend(ids);
                    }
                    log.open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        addr
    }

    /// An address nothing listens on, so connections are refused at once.
    async fn refusing() -> SocketAddrV4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = listener.local_addr().unwrap() else { unreachable!() };
        addr
    }

    async fn udp() -> (UdpSocket, SocketAddrV4) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = socket.local_addr().unwrap() else { unreachable!() };
        (socket, addr)
    }

    /// Runs the fake nodes, and a client whose bootstrap router introduces
    /// the first of them.
    async fn client(nodes: Vec<(UdpSocket, Node)>, seen: &Arc<Seen>, cfg: DhtConfig) -> Dht {
        let SocketAddr::V4(addr) = nodes[0].0.local_addr().unwrap() else { unreachable!() };
        let first = NodeInfo { id: nodes[0].1.id, addr };
        for (socket, node) in nodes {
            tokio::spawn(fake_node(socket, node, seen.clone()));
        }
        let (router, router_addr) = udp().await;
        tokio::spawn(fake_node(router, Node { id: [0x24; 20], nodes: vec![first], ..Node::default() }, seen.clone()));
        let cfg = DhtConfig { bind: "127.0.0.1:0".parse().unwrap(), bootstrap: vec![router_addr.to_string()], ..cfg };
        Dht::start_with(cfg, true).await.unwrap()
    }

    /// Nodes in a chain, each closer to the info hash than the last, each
    /// answering get_peers after `delay` with the next node and its `peers`.
    /// A lookup walks it one node at a time, so it takes `delay` per node.
    async fn chain(info_hash: [u8; 20], peers: Vec<Vec<SocketAddrV4>>, delay: Duration) -> Vec<(UdpSocket, Node)> {
        let mut sockets = Vec::new();
        for i in 0..peers.len() {
            let mut id = info_hash;
            id[1 + i / 8] ^= 0x80 >> (i % 8);
            let (socket, addr) = udp().await;
            sockets.push((socket, NodeInfo { id, addr }));
        }
        let infos: Vec<NodeInfo> = sockets.iter().map(|(_, info)| *info).collect();
        sockets
            .into_iter()
            .zip(peers)
            .enumerate()
            .map(|(i, ((socket, me), peers))| {
                let closer = infos.get(i + 1).copied().into_iter().collect();
                (socket, Node { id: me.id, info_hash, closer, peers, delay, ..Node::default() })
            })
            .collect()
    }

    /// A client of a DHT with one node, which knows `peers`.
    async fn one_node(info_hash: [u8; 20], peers: Vec<SocketAddrV4>, cfg: DhtConfig) -> Dht {
        let node = Node { id: [0x42; 20], info_hash, peers, ..Node::default() };
        client(vec![(udp().await.0, node)], &Arc::new(Seen::default()), cfg).await
    }

    /// A two-file torrent whose info dict spans two metadata pieces.
    fn fixture() -> (Vec<u8>, [u8; 20]) {
        let pieces = vec![0x5a; 20 * 1000];
        let file = |len: i64, path: &'static [u8]| {
            Value::dict(vec![("length", Value::Int(len)), ("path", Value::List(vec![Value::Bytes(path)]))])
        };
        let info = bencode::encode(&Value::dict(vec![
            ("files", Value::List(vec![file(1 << 30, b"film.mkv"), file(2048, b"film.srt")])),
            ("name", Value::Bytes(b"Fixture Film")),
            ("piece length", Value::Int(1 << 20)),
            ("pieces", Value::Bytes(&pieces)),
        ]));
        let hash = meta::info_hash(&info);
        (info, hash)
    }

    #[tokio::test]
    async fn resolves_metadata_and_harvests_over_loopback() {
        let (info, info_hash) = fixture();
        assert!(info.len() > peer::PIECE);
        let log = Arc::new(PeerLog::default());
        let good = fake_peer(&info, info_hash, Behavior::Good, log.clone()).await;
        let corrupt = fake_peer(&info, info_hash, Behavior::Corrupt, log.clone()).await;

        let seen = Arc::new(Seen::default());
        let samples = vec![[1; 20], [2; 20], [3; 20]];
        let node =
            Node { id: [0x42; 20], info_hash, peers: vec![good, corrupt], samples: samples.clone(), ..Node::default() };
        let dht = client(vec![(udp().await.0, node)], &seen, DhtConfig::default()).await;

        let meta = dht.metadata(info_hash).await.unwrap();
        assert_eq!(meta.name, "Fixture Film");
        assert_eq!(
            meta.files,
            vec![FileMeta { path: "film.mkv".into(), size: 1 << 30 }, FileMeta { path: "film.srt".into(), size: 2048 }]
        );
        assert_eq!(
            (meta.total_size, meta.piece_length, meta.private, meta.peers),
            ((1 << 30) + 2048, 1 << 20, false, 2)
        );

        let mut peers = dht.get_peers(info_hash).await.unwrap();
        peers.sort();
        let mut expected = vec![SocketAddr::V4(good), SocketAddr::V4(corrupt)];
        expected.sort();
        assert_eq!(peers, expected);
        assert!(matches!(dht.metadata([9; 20]).await, Err(DhtError::NoPeers)));

        let status = dht.status();
        assert!(status.ready, "{status:?}");
        assert_eq!(status.nodes, 1, "the router stays out of the table: {status:?}");
        assert!(status.responses > 0 && status.queries_sent >= status.responses, "{status:?}");

        let mut harvest = dht.harvest();
        let mut got = HashSet::new();
        while got.len() < samples.len() {
            let hash = tokio::time::timeout(Duration::from_secs(10), harvest.recv()).await.unwrap().unwrap();
            got.insert(hash);
        }
        assert_eq!(got, samples.into_iter().collect());
        // The node asked for an hour between samples.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(seen.samples_asked.load(Ordering::SeqCst), 1);
        drop(harvest);

        assert!(!seen.not_read_only.load(Ordering::SeqCst), "every query must carry ro=1");
        let forbidden = log.forbidden.lock().unwrap();
        assert!(forbidden.is_empty(), "sent non-extension messages: {forbidden:?}");
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[tokio::test]
    async fn does_not_wait_for_slow_or_broken_peers() {
        let (info, info_hash) = fixture();
        let log = Arc::new(PeerLog::default());
        let mut peers = Vec::new();
        use Behavior::*;
        for behavior in [Stall(HOUR), Stall(HOUR), Stall(HOUR), Garbage, Corrupt, Reject, Good] {
            peers.push(fake_peer(&info, info_hash, behavior, log.clone()).await);
        }
        peers.push(SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 80)); // a low port: neither tried nor counted
        let dht = one_node(info_hash, peers, DhtConfig::default()).await;

        let start = Instant::now();
        let meta = dht.metadata(info_hash).await.unwrap();
        // A stalled peer holds its connection for the 10 s read timeout.
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        assert_eq!((meta.name.as_str(), meta.peers), ("Fixture Film", 7));
        assert!(log.all_closed().await, "{} connections left open", log.open.load(Ordering::SeqCst));
        assert!(log.forbidden.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tries_peers_while_the_lookup_goes_on() {
        // Each node of the chain names one peer, and the walk takes 15 s. The
        // first two peers fail at once, which must not end the search while
        // the lookup is still finding more; the third stalls; the fourth has
        // the metadata.
        const CHAIN: usize = 30;
        let (info, info_hash) = fixture();
        let log = Arc::new(PeerLog::default());
        let mut peers = vec![
            fake_peer(&info, info_hash, Behavior::Garbage, log.clone()).await,
            refusing().await,
            fake_peer(&info, info_hash, Behavior::Stall(HOUR), log.clone()).await,
            fake_peer(&info, info_hash, Behavior::Good, log.clone()).await,
        ];
        while peers.len() < CHAIN {
            peers.push(refusing().await);
        }
        let nodes = chain(info_hash, peers.into_iter().map(|p| vec![p]).collect(), Duration::from_millis(500)).await;
        let seen = Arc::new(Seen::default());
        let dht = client(nodes, &seen, DhtConfig::default()).await;

        let start = Instant::now();
        let meta = dht.metadata(info_hash).await.unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(8), "waited for the lookup: {elapsed:?}");
        assert_eq!(meta.name, "Fixture Film");
        // Counted on for a while after the metadata came, but not to the end.
        assert!((5..CHAIN).contains(&meta.peers), "{} peers", meta.peers);
        assert!(log.all_closed().await, "{} connections left open", log.open.load(Ordering::SeqCst));
        let asked = seen.get_peers_asked.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(seen.get_peers_asked.load(Ordering::SeqCst), asked, "the lookup went on after metadata()");
        assert!(asked < CHAIN, "{asked}");
    }

    #[tokio::test]
    async fn a_lookup_that_never_ends_gets_half_the_time() {
        // A 15 s walk that finds no peers, under a 4 s limit: after 2 s the
        // answer is that there are none, not a timeout.
        let info_hash = [0x33; 20];
        let nodes = chain(info_hash, vec![Vec::new(); 30], Duration::from_millis(500)).await;
        let cfg = DhtConfig { metadata_timeout: Duration::from_secs(4), ..DhtConfig::default() };
        let dht = client(nodes, &Arc::new(Seen::default()), cfg).await;
        let start = Instant::now();
        let result = dht.metadata(info_hash).await;
        let elapsed = start.elapsed();
        assert!(matches!(result, Err(DhtError::NoPeers)), "{result:?} after {elapsed:?}");
        assert!(elapsed >= Duration::from_millis(1900) && elapsed < Duration::from_secs(3), "{elapsed:?}");
    }

    #[tokio::test]
    async fn never_opens_more_than_max_peer_connections() {
        let (info, info_hash) = fixture();
        let log = Arc::new(PeerLog::default());
        let mut peers = Vec::new();
        for _ in 0..6 {
            peers.push(fake_peer(&info, info_hash, Behavior::Stall(Duration::from_millis(300)), log.clone()).await);
        }
        peers.push(fake_peer(&info, info_hash, Behavior::Good, log.clone()).await);
        let dht = one_node(info_hash, peers, DhtConfig { max_peer_connections: 2, ..DhtConfig::default() }).await;
        let meta = dht.metadata(info_hash).await.unwrap();
        assert_eq!(meta.peers, 7);
        assert!(log.most_open.load(Ordering::SeqCst) <= 2, "{} at once", log.most_open.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn says_why_no_peer_delivered() {
        let (info, info_hash) = fixture();
        let log = Arc::new(PeerLog::default());
        let broken = vec![
            fake_peer(&info, info_hash, Behavior::Garbage, log.clone()).await,
            fake_peer(&info, info_hash, Behavior::Corrupt, log.clone()).await,
            refusing().await,
        ];
        let result = one_node(info_hash, broken, DhtConfig::default()).await.metadata(info_hash).await;
        assert!(matches!(result, Err(DhtError::PeersFailed { tried: 3, .. })), "{result:?}");

        let oversize = vec![
            fake_peer(&info, info_hash, Behavior::Oversize(1 << 40), log.clone()).await,
            fake_peer(&info, info_hash, Behavior::Garbage, log.clone()).await,
        ];
        let result = one_node(info_hash, oversize, DhtConfig::default()).await.metadata(info_hash).await;
        assert!(matches!(result, Err(DhtError::TooLarge(size)) if size == 1 << 40), "{result:?}");

        let stalled = vec![fake_peer(&info, info_hash, Behavior::Stall(HOUR), log.clone()).await];
        let cfg = DhtConfig { metadata_timeout: Duration::from_secs(1), ..DhtConfig::default() };
        let dht = one_node(info_hash, stalled, cfg).await;
        let start = Instant::now();
        let result = dht.metadata(info_hash).await;
        assert!(matches!(result, Err(DhtError::Timeout(t)) if t == Duration::from_secs(1)), "{result:?}");
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        assert!(log.all_closed().await, "{} connections left open", log.open.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn refuses_ipv6_binds() {
        let cfg = DhtConfig { bind: "[::1]:0".parse().unwrap(), ..DhtConfig::default() };
        assert_eq!(Dht::start(cfg).await.err().map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
    }
}

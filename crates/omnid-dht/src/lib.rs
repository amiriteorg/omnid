//! A read-only BitTorrent Mainline DHT client, for two jobs:
//!
//! - [`Dht::metadata`] resolves a magnet's info hash to the torrent's name,
//!   files and sizes, and estimates the swarm, without downloading content.
//!   A `get_peers` lookup (BEP 5) finds peers; each is asked for the info
//!   dict over `ut_metadata` (BEP 9), and the result is checked against the
//!   info hash.
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
use rand::seq::SliceRandom;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::lookup::Kind;
use crate::peer::PeerError;
use crate::routing::K;
use crate::rpc::Core;

/// The longest a get_peers lookup runs.
const LOOKUP_TIME: Duration = Duration::from_secs(30);
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
    /// Peers found by the lookup: a lower bound on the swarm's size.
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

    /// get_peers, then BEP 9 ut_metadata from peers; SHA-1 verified against
    /// info_hash.
    pub async fn metadata(&self, info_hash: [u8; 20]) -> Result<TorrentMeta, DhtError> {
        let limit = self.inner.core.cfg.metadata_timeout;
        tokio::time::timeout(limit, self.fetch_metadata(info_hash)).await.map_err(|_| DhtError::Timeout(limit))?
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

    async fn lookup_peers(&self, info_hash: [u8; 20], budget: Duration) -> Result<Vec<SocketAddrV4>, DhtError> {
        let core = &self.inner.core;
        if !core.wait_ready(READY_WAIT).await {
            return Err(DhtError::NotReady);
        }
        let seeds = core.closest(&info_hash, 2 * K);
        if seeds.is_empty() {
            return Err(DhtError::NotReady);
        }
        let outcome = lookup::lookup(core, info_hash, Kind::GetPeers, seeds, Instant::now() + budget).await;
        tracing::debug!(
            info_hash = %hex(&info_hash),
            peers = outcome.peers.len(),
            queried = outcome.queried,
            answered = outcome.answered,
            "dht: get_peers lookup done"
        );
        Ok(outcome.peers)
    }

    async fn fetch_metadata(&self, info_hash: [u8; 20]) -> Result<TorrentMeta, DhtError> {
        let core = &self.inner.core;
        // Leave at least half the time for the peers.
        let mut peers = self.lookup_peers(info_hash, (core.cfg.metadata_timeout / 2).min(LOOKUP_TIME)).await?;
        if peers.is_empty() {
            return Err(DhtError::NoPeers);
        }
        let found = peers.len();
        peers.shuffle(&mut rand::rng());
        let mut queue = peers.into_iter();
        let mut tasks = JoinSet::new();
        let (mut tried, mut too_large, mut last) = (0, None, String::from("no peer answered"));
        loop {
            while tasks.len() < core.cfg.max_peer_connections.max(1) {
                let Some(peer) = queue.next() else {
                    break;
                };
                tried += 1;
                let (peer_id, max) = (core.peer_id, core.cfg.max_metadata_bytes);
                tasks.spawn(async move {
                    let fetch = peer::fetch_metadata(SocketAddr::V4(peer), &info_hash, &peer_id, max);
                    (peer, tokio::time::timeout(PEER_TIME, fetch).await.unwrap_or(Err(PeerError::Timeout)))
                });
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            match joined {
                Ok((peer, Ok(info))) => {
                    // These bytes hash to the info hash, so they are the
                    // torrent's info dict: no other peer can do better.
                    let mut meta = meta::parse_info(&info).map_err(DhtError::BadMetadata)?;
                    meta.peers = found;
                    tracing::debug!(
                        info_hash = %hex(&info_hash),
                        %peer,
                        name = %meta.name,
                        files = meta.files.len(),
                        "dht: got metadata"
                    );
                    // Dropping `tasks` closes the other connections.
                    return Ok(meta);
                }
                Ok((peer, Err(e))) => {
                    tracing::trace!(%peer, "dht: no metadata from peer: {e}");
                    if let PeerError::TooLarge(size) = e {
                        too_large = Some(size);
                    }
                    last = e.to_string();
                }
                Err(e) => last = format!("peer task failed: {e}"),
            }
        }
        match too_large {
            Some(size) => Err(DhtError::TooLarge(size)),
            None => Err(DhtError::PeersFailed { tried, last }),
        }
    }
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
    }

    /// Answers like a DHT node: `nodes` to everything, `peers` to get_peers
    /// for `info_hash`, `samples` to sample_infohashes.
    async fn fake_node(
        socket: UdpSocket,
        id: [u8; 20],
        nodes: Vec<NodeInfo>,
        peers: Vec<SocketAddrV4>,
        samples: Vec<[u8; 20]>,
        info_hash: [u8; 20],
        seen: Arc<Seen>,
    ) {
        let mut buf = [0; 2048];
        while let Ok((len, from)) = socket.recv_from(&mut buf).await {
            let Some(query) = decode_query(&buf[..len]) else {
                continue;
            };
            if !query.read_only {
                seen.not_read_only.store(true, Ordering::SeqCst);
            }
            let mut response = Response { id, nodes: nodes.clone(), ..Default::default() };
            match query.query {
                Query::GetPeers { info_hash: wanted } if wanted == info_hash => response.values = peers.clone(),
                Query::SampleInfohashes { .. } => {
                    seen.samples_asked.fetch_add(1, Ordering::SeqCst);
                    response.samples = samples.clone();
                    response.interval = Some(3600);
                    response.num = Some(samples.len() as u64);
                }
                _ => {}
            }
            socket.send_to(&encode_response(&query.tid, &response), from).await.unwrap();
        }
    }

    async fn fake_peer(
        info: &[u8],
        info_hash: [u8; 20],
        behavior: Behavior,
        forbidden: Arc<Mutex<Vec<u8>>>,
    ) -> SocketAddrV4 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = listener.local_addr().unwrap() else { unreachable!() };
        let info = info.to_vec();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (info, forbidden) = (info.clone(), forbidden.clone());
                tokio::spawn(async move {
                    if let Ok(ids) = serve(stream, info_hash, info, behavior).await {
                        forbidden.lock().unwrap().extend(ids);
                    }
                });
            }
        });
        addr
    }

    async fn udp() -> (UdpSocket, SocketAddrV4) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = socket.local_addr().unwrap() else { unreachable!() };
        (socket, addr)
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
        let forbidden = Arc::new(Mutex::new(Vec::new()));
        let good = fake_peer(&info, info_hash, Behavior::Good, forbidden.clone()).await;
        let corrupt = fake_peer(&info, info_hash, Behavior::Corrupt, forbidden.clone()).await;

        let seen = Arc::new(Seen::default());
        let samples = vec![[1; 20], [2; 20], [3; 20]];
        let (node, node_addr) = udp().await;
        let (router, router_addr) = udp().await;
        tokio::spawn(fake_node(
            node,
            [0x42; 20],
            vec![],
            vec![good, corrupt],
            samples.clone(),
            info_hash,
            seen.clone(),
        ));
        let introduced = vec![NodeInfo { id: [0x42; 20], addr: node_addr }];
        tokio::spawn(fake_node(router, [0x24; 20], introduced, vec![], vec![], info_hash, seen.clone()));

        let cfg = DhtConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            bootstrap: vec![router_addr.to_string()],
            ..DhtConfig::default()
        };
        let dht = Dht::start_with(cfg, true).await.unwrap();

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
        assert!(forbidden.lock().unwrap().is_empty(), "sent non-extension messages: {forbidden:?}");
    }

    #[tokio::test]
    async fn refuses_ipv6_binds() {
        let cfg = DhtConfig { bind: "[::1]:0".parse().unwrap(), ..DhtConfig::default() };
        assert_eq!(Dht::start(cfg).await.err().map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
    }
}

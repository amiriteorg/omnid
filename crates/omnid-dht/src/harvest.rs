//! BEP 51 harvesting: asks DHT nodes for samples of the info hashes they
//! store, walking from node to node through the `nodes` each answer
//! carries. A node is asked again only after the `interval` it gave.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::krpc::{NodeInfo, Query};
use crate::rpc::{Core, QueryError};

/// Queries in flight. The harvester shares the global rate limit with
/// lookups, and this keeps it from crowding them out.
const CONCURRENCY: usize = 8;
const MAX_QUEUE: usize = 10_000;
/// Nodes remembered with their next allowed visit.
const MAX_TRACKED: usize = 1_000_000;
/// BEP 51 caps `interval` at six hours. Nodes that cannot answer
/// sample_infohashes are left alone that long too.
const MAX_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const MIN_INTERVAL: Duration = Duration::from_secs(60);
/// Info hashes remembered to drop repeats, per generation.
const RECENT: usize = 100_000;
const IDLE: Duration = Duration::from_secs(2);
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// Harvests until the receiver is dropped.
pub(crate) async fn run(core: &Arc<Core>, tx: mpsc::Sender<[u8; 20]>) {
    let mut walk = Walk::default();
    let mut recent = Recent::default();
    let mut tasks = JoinSet::new();
    let mut sent = 0u64;
    loop {
        let now = Instant::now();
        if walk.queue.is_empty() {
            for node in core.table_nodes() {
                walk.offer(node.addr, now);
            }
        }
        while tasks.len() < CONCURRENCY {
            let Some(addr) = walk.queue.pop_front() else {
                break;
            };
            let core = core.clone();
            let target = rand::random();
            tasks.spawn(async move { (addr, core.query(addr, &Query::SampleInfohashes { target }).await) });
        }
        let joined = tokio::select! {
            _ = tx.closed() => break,
            joined = tasks.join_next(), if !tasks.is_empty() => joined,
            _ = tokio::time::sleep(IDLE), if tasks.is_empty() => continue,
        };
        let Some(Ok((addr, result))) = joined else {
            continue;
        };
        let now = Instant::now();
        match result {
            Ok(response) => {
                core.heard_from(NodeInfo { id: response.id, addr });
                walk.visited(addr, now + revisit_after(response.interval, !response.samples.is_empty()));
                for node in response.nodes {
                    if node.id != core.id && core.usable_node(node.addr) {
                        walk.offer(node.addr, now);
                    }
                }
                for hash in response.samples {
                    if recent.insert(hash) {
                        if tx.send(hash).await.is_err() {
                            return;
                        }
                        sent += 1;
                    }
                }
            }
            Err(e) => {
                if !matches!(e, QueryError::Remote { .. }) {
                    core.failed(addr);
                }
                walk.visited(addr, now + MAX_INTERVAL);
            }
        }
    }
    tracing::debug!(sent, "dht: harvest stopped");
}

/// How long a node that answered is left alone. A node that answers
/// without `interval` or `samples` took the query for something it knows,
/// so it does not speak BEP 51.
fn revisit_after(interval: Option<u64>, has_samples: bool) -> Duration {
    match interval {
        Some(seconds) => Duration::from_secs(seconds).clamp(MIN_INTERVAL, MAX_INTERVAL),
        None if !has_samples => MAX_INTERVAL,
        None => MIN_INTERVAL,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visit {
    /// Waiting in the queue or being asked.
    Queued,
    NotBefore(Instant),
}

/// Which nodes to ask next, and when each may be asked again.
#[derive(Default)]
struct Walk {
    queue: VecDeque<SocketAddrV4>,
    visits: HashMap<SocketAddrV4, Visit>,
    last_prune: Option<Instant>,
}

impl Walk {
    /// Queues a node unless it is queued already or asked us to wait.
    fn offer(&mut self, addr: SocketAddrV4, now: Instant) {
        if self.queue.len() >= MAX_QUEUE {
            return;
        }
        match self.visits.get(&addr) {
            Some(Visit::Queued) => return,
            Some(Visit::NotBefore(t)) if *t > now => return,
            _ => {}
        }
        if self.visits.len() >= MAX_TRACKED {
            self.prune(now);
            // Forgetting a node would mean asking it again too soon.
            if self.visits.len() >= MAX_TRACKED {
                return;
            }
        }
        self.visits.insert(addr, Visit::Queued);
        self.queue.push_back(addr);
    }

    fn visited(&mut self, addr: SocketAddrV4, not_before: Instant) {
        self.visits.insert(addr, Visit::NotBefore(not_before));
    }

    fn prune(&mut self, now: Instant) {
        if self.last_prune.is_some_and(|t| now.duration_since(t) < PRUNE_EVERY) {
            return;
        }
        self.last_prune = Some(now);
        self.visits.retain(|_, visit| match visit {
            Visit::Queued => true,
            Visit::NotBefore(t) => *t > now,
        });
    }
}

/// Recently seen info hashes, in two generations so memory stays bounded.
#[derive(Default)]
struct Recent {
    current: HashSet<[u8; 20]>,
    previous: HashSet<[u8; 20]>,
}

impl Recent {
    /// Whether the hash is new.
    fn insert(&mut self, hash: [u8; 20]) -> bool {
        if self.current.contains(&hash) || self.previous.contains(&hash) {
            return false;
        }
        if self.current.len() >= RECENT {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(hash)
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::net::UdpSocket;

    use super::*;
    use crate::DhtConfig;
    use crate::krpc::Response;
    use crate::krpc::testing::{decode_query, encode_error, encode_response};

    fn addr(port: u16) -> SocketAddrV4 {
        SocketAddrV4::new([1, 1, 1, 1].into(), port)
    }

    #[test]
    fn honors_each_nodes_interval() {
        let start = Instant::now();
        let mut walk = Walk::default();
        walk.offer(addr(1), start);
        walk.offer(addr(1), start);
        assert_eq!(walk.queue.len(), 1, "queued once");
        let a = walk.queue.pop_front().unwrap();
        walk.offer(a, start);
        assert!(walk.queue.is_empty(), "not queued again while being asked");

        walk.visited(a, start + Duration::from_secs(600));
        walk.offer(a, start + Duration::from_secs(599));
        assert!(walk.queue.is_empty(), "not before its interval");
        walk.offer(a, start + Duration::from_secs(600));
        assert_eq!(walk.queue.len(), 1, "due again once the interval has passed");
    }

    #[test]
    fn bounds_the_queue() {
        let now = Instant::now();
        let mut walk = Walk::default();
        for port in 0..=MAX_QUEUE as u16 {
            walk.offer(addr(port), now);
        }
        assert_eq!(walk.queue.len(), MAX_QUEUE);
    }

    #[test]
    fn drops_repeated_hashes() {
        let mut recent = Recent::default();
        assert!(recent.insert([1; 20]));
        assert!(!recent.insert([1; 20]));
        assert!(recent.insert([2; 20]));
    }

    #[test]
    fn keeps_intervals_within_bep_51s_bounds() {
        assert_eq!(revisit_after(Some(0), true), MIN_INTERVAL, "no wait at all is a minute");
        assert_eq!(revisit_after(Some(59), true), MIN_INTERVAL);
        assert_eq!(revisit_after(Some(3600), true), Duration::from_secs(3600));
        assert_eq!(revisit_after(Some(3600), false), Duration::from_secs(3600), "an empty sample is still BEP 51");
        assert_eq!(revisit_after(Some(6 * 60 * 60 + 1), true), MAX_INTERVAL);
        assert_eq!(revisit_after(Some(u64::MAX), true), MAX_INTERVAL);
        assert_eq!(revisit_after(None, true), MIN_INTERVAL);
        assert_eq!(revisit_after(None, false), MAX_INTERVAL, "a node that does not speak BEP 51");
    }

    #[test]
    fn prunes_expired_visits_at_most_once_a_minute() {
        let start = Instant::now();
        let mut walk = Walk::default();
        walk.offer(addr(1), start);
        walk.visited(addr(2), start + Duration::from_secs(10));
        walk.visited(addr(3), start + Duration::from_secs(1000));
        walk.prune(start + Duration::from_secs(20));
        assert!(!walk.visits.contains_key(&addr(2)), "its interval is over");
        assert!(walk.visits.contains_key(&addr(1)) && walk.visits.contains_key(&addr(3)), "queued, and not due yet");

        walk.visited(addr(4), start + Duration::from_secs(30));
        walk.prune(start + Duration::from_secs(40));
        assert!(walk.visits.contains_key(&addr(4)), "pruned again too soon");
        walk.prune(start + Duration::from_secs(20) + PRUNE_EVERY);
        assert!(!walk.visits.contains_key(&addr(4)));
        assert_eq!(walk.visits.len(), 2);
    }

    #[test]
    fn forgets_hashes_two_generations_back() {
        let hash = |i: usize| {
            let mut hash = [0; 20];
            hash[..8].copy_from_slice(&(i as u64).to_le_bytes());
            hash
        };
        let mut recent = Recent::default();
        for i in 0..RECENT {
            assert!(recent.insert(hash(i)));
        }
        assert!(recent.insert(hash(RECENT)), "starts a new generation");
        assert!(!recent.insert(hash(0)), "the previous generation still counts");
        for i in RECENT + 1..2 * RECENT {
            assert!(recent.insert(hash(i)));
        }
        assert!(recent.insert(hash(2 * RECENT)));
        assert!(recent.insert(hash(0)), "two generations back is forgotten");
        assert!(recent.current.len() + recent.previous.len() <= 2 * RECENT);
    }

    /// A node that answers every query with `reply(tid)`, and counts them.
    async fn node(reply: impl Fn(&[u8]) -> Vec<u8> + Send + 'static) -> (SocketAddrV4, Arc<AtomicUsize>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(addr) = socket.local_addr().unwrap() else { unreachable!() };
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        tokio::spawn(async move {
            let mut buf = [0; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut buf).await {
                let Some(query) = decode_query(&buf[..len]) else {
                    continue;
                };
                count.fetch_add(1, Ordering::SeqCst);
                socket.send_to(&reply(&query.tid), from).await.unwrap();
            }
        });
        (addr, asked)
    }

    #[tokio::test]
    async fn asks_each_node_once_per_interval_over_loopback() {
        let cfg = DhtConfig { bind: "127.0.0.1:0".parse().unwrap(), ..DhtConfig::default() };
        let core = Arc::new(Core::bind(cfg, true).await.unwrap());
        let receiver = tokio::spawn(core.clone().receive());
        // One asks for no wait at all, one does not speak BEP 51, one answers
        // with an error. Asking any of them again now would be too soon.
        let eager = node(|tid| {
            let r = Response { id: [1; 20], samples: vec![[0xaa; 20]], interval: Some(0), ..Default::default() };
            encode_response(tid, &r)
        })
        .await;
        let legacy = node(|tid| encode_response(tid, &Response { id: [2; 20], ..Default::default() })).await;
        let erroring = node(|tid| encode_error(tid, 204, "Method Unknown")).await;
        for (i, (addr, _)) in [&eager, &legacy, &erroring].into_iter().enumerate() {
            core.heard_from(NodeInfo { id: [i as u8 + 1; 20], addr: *addr });
        }

        let (tx, mut rx) = mpsc::channel(16);
        let harvester = tokio::spawn({
            let core = core.clone();
            async move { run(&core, tx).await }
        });
        let hash = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap();
        assert_eq!(hash, Some([0xaa; 20]));
        tokio::time::sleep(Duration::from_millis(500)).await;
        for (name, (_, asked)) in [("eager", &eager), ("legacy", &legacy), ("erroring", &erroring)] {
            assert_eq!(asked.load(Ordering::SeqCst), 1, "{name}");
        }
        assert!(rx.try_recv().is_err(), "the same sample is not sent twice");
        assert_eq!(core.table_len(), 3, "an error is an answer: the node stays in the table");

        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), harvester).await.unwrap().unwrap();
        receiver.abort();
    }
}

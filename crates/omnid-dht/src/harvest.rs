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
                // A node that answers without `interval` or `samples` took the
                // query for something it knows, so it does not speak BEP 51.
                let wait = match response.interval {
                    Some(seconds) => Duration::from_secs(seconds).clamp(MIN_INTERVAL, MAX_INTERVAL),
                    None if response.samples.is_empty() => MAX_INTERVAL,
                    None => MIN_INTERVAL,
                };
                walk.visited(addr, now + wait);
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
    use super::*;

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
}

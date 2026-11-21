//! Iterative Kademlia lookups (find_node, get_peers), the bootstrap, and the
//! upkeep that keeps the routing table alive.

use std::collections::{BTreeMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinSet;

use crate::krpc::{NodeId, NodeInfo, Query};
use crate::routing::{K, distance};
use crate::rpc::{Core, QueryError};

/// Queries in flight per lookup.
const ALPHA: usize = 8;
/// Queries one lookup may send in all.
const MAX_QUERIES: usize = 128;
const MAX_CANDIDATES: usize = 256;
/// Peers one lookup collects. Hostile nodes can return any number.
const MAX_PEERS: usize = 5000;
/// A query unanswered this long gives up its place among the `K` closest
/// and its slot in `ALPHA`, so a dead node does not stall the lookup for
/// the whole query timeout. A late answer still counts.
const SOFT_TIMEOUT: Duration = Duration::from_secs(1);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_TIME: Duration = Duration::from_secs(30);
const UPKEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Nodes not heard from for this long are pinged.
const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// A lookup for a random ID this often keeps the far buckets populated.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
const RETRY_MIN: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    FindNode,
    GetPeers,
}

#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// get_peers: deduplicated peers, in no particular order.
    pub peers: Vec<SocketAddrV4>,
    pub queried: usize,
    pub answered: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Fresh,
    Asked(Instant),
    Answered,
    Failed,
}

/// Lookup candidates, closest to the target first.
struct Candidates {
    target: NodeId,
    own: NodeId,
    by_distance: BTreeMap<NodeId, (SocketAddrV4, State)>,
    seen: HashSet<SocketAddrV4>,
}

impl Candidates {
    fn add(&mut self, node: NodeInfo) {
        if node.id == self.own || !self.seen.insert(node.addr) {
            return;
        }
        self.by_distance.insert(distance(&node.id, &self.target), (node.addr, State::Fresh));
        if self.by_distance.len() > MAX_CANDIDATES {
            self.by_distance.pop_last();
        }
    }

    /// The closest candidate not asked yet, if it is among the `K` closest
    /// that have neither failed nor gone slow. Once those have all
    /// answered, the lookup has converged.
    fn next(&mut self, now: Instant) -> Option<(NodeId, SocketAddrV4)> {
        let (key, (addr, state)) = self
            .by_distance
            .iter_mut()
            .filter(|(_, (_, state))| match state {
                State::Failed => false,
                State::Asked(at) => now.duration_since(*at) < SOFT_TIMEOUT,
                State::Fresh | State::Answered => true,
            })
            .take(K)
            .find(|(_, (_, state))| *state == State::Fresh)?;
        *state = State::Asked(now);
        Some((*key, *addr))
    }

    /// Queries in flight that have not gone slow.
    fn active(&self, now: Instant) -> usize {
        self.by_distance
            .values()
            .filter(|(_, state)| matches!(state, State::Asked(at) if now.duration_since(*at) < SOFT_TIMEOUT))
            .count()
    }

    fn set(&mut self, key: &NodeId, state: State) {
        if let Some((_, s)) = self.by_distance.get_mut(key) {
            *s = state;
        }
    }
}

/// Walks toward `target`, starting from `seeds`, until the `K` closest
/// nodes found have answered, the query budget is spent or `deadline`
/// passes. Every node that answers goes into the routing table.
pub(crate) async fn lookup(
    core: &Arc<Core>,
    target: NodeId,
    kind: Kind,
    seeds: Vec<NodeInfo>,
    deadline: Instant,
) -> Outcome {
    let query = match kind {
        Kind::FindNode => Query::FindNode { target },
        Kind::GetPeers => Query::GetPeers { info_hash: target },
    };
    let mut candidates = Candidates { target, own: core.id, by_distance: BTreeMap::new(), seen: HashSet::new() };
    for seed in seeds {
        if core.usable_node(seed.addr) {
            candidates.add(seed);
        }
    }
    let mut outcome = Outcome::default();
    let mut peers = HashSet::new();
    let mut tasks = JoinSet::new();
    loop {
        let now = Instant::now();
        while candidates.active(now) < ALPHA
            && tasks.len() < 2 * ALPHA
            && outcome.queried < MAX_QUERIES
            && now < deadline
        {
            let Some((key, addr)) = candidates.next(now) else {
                break;
            };
            outcome.queried += 1;
            let (core, query) = (core.clone(), query.clone());
            tasks.spawn(async move { (key, addr, core.query(addr, &query).await) });
        }
        let joined = tokio::select! {
            joined = tasks.join_next() => joined,
            // Lets slow queries hand their places on.
            _ = tokio::time::sleep(SOFT_TIMEOUT / 4), if !tasks.is_empty() => continue,
            _ = tokio::time::sleep_until(deadline.into()) => break,
        };
        let Some(joined) = joined else {
            break;
        };
        let Ok((key, addr, result)) = joined else {
            continue;
        };
        match result {
            Ok(response) => {
                candidates.set(&key, State::Answered);
                outcome.answered += 1;
                core.heard_from(NodeInfo { id: response.id, addr });
                for node in response.nodes {
                    if core.usable_node(node.addr) {
                        candidates.add(node);
                    }
                }
                if kind == Kind::GetPeers {
                    for peer in response.values {
                        if peers.len() < MAX_PEERS && core.usable_peer(peer) {
                            peers.insert(peer);
                        }
                    }
                }
            }
            Err(e) => {
                candidates.set(&key, State::Failed);
                if !matches!(e, QueryError::Remote { .. }) {
                    core.failed(addr);
                }
                tracing::trace!(%addr, "dht: {} failed: {e}", query.method());
            }
        }
    }
    outcome.peers = peers.into_iter().collect();
    outcome
}

/// Asks the bootstrap routers for nodes near our ID, then looks our ID up
/// to fill the table. Returns the table size.
pub(crate) async fn bootstrap(core: &Arc<Core>) -> usize {
    let mut routers = Vec::new();
    for host in &core.cfg.bootstrap {
        match tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host(host.as_str())).await {
            Ok(Ok(addrs)) => routers.extend(addrs.filter_map(|addr| match addr {
                SocketAddr::V4(addr) => Some(addr),
                SocketAddr::V6(_) => None,
            })),
            Ok(Err(e)) => tracing::debug!(host, "dht: cannot resolve bootstrap node: {e}"),
            Err(_) => tracing::debug!(host, "dht: resolving bootstrap node timed out"),
        }
    }
    routers.sort();
    routers.dedup();
    core.set_routers(&routers);

    let mut asks = JoinSet::new();
    for router in routers {
        let core = core.clone();
        asks.spawn(async move { core.query(router, &Query::FindNode { target: core.id }).await });
    }
    let mut seeds = core.closest(&core.id, 2 * K);
    while let Some(joined) = asks.join_next().await {
        if let Ok(Ok(response)) = joined {
            seeds.extend(response.nodes);
        }
    }
    lookup(core, core.id, Kind::FindNode, seeds, Instant::now() + BOOTSTRAP_TIME).await;
    core.table_len()
}

/// Bootstraps, then keeps the table healthy: pings nodes gone quiet so dead
/// ones fall out, refreshes with a random lookup now and then, and
/// bootstraps again, backing off, if the table runs low.
pub(crate) async fn upkeep(core: Arc<Core>) {
    let mut retry = RETRY_MIN;
    let mut last_refresh = Instant::now();
    loop {
        if core.table_len() < K {
            let nodes = bootstrap(&core).await;
            core.set_ready(nodes > 0);
            if nodes == 0 {
                tracing::warn!("dht: no node answered the bootstrap; retrying in {retry:?}");
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
                continue;
            }
            tracing::info!(nodes, "dht: bootstrapped");
            retry = RETRY_MIN;
        }
        tokio::time::sleep(UPKEEP_INTERVAL).await;

        if let Some(before) = Instant::now().checked_sub(STALE_AFTER) {
            let mut pings = JoinSet::new();
            for node in core.stale(before, K) {
                let core = core.clone();
                pings.spawn(async move { (node.addr, core.query(node.addr, &Query::Ping).await) });
            }
            while let Some(joined) = pings.join_next().await {
                match joined {
                    Ok((addr, Ok(response))) => core.heard_from(NodeInfo { id: response.id, addr }),
                    Ok((addr, Err(QueryError::Timeout | QueryError::Io(_)))) => core.failed(addr),
                    _ => {}
                }
            }
        }
        if last_refresh.elapsed() >= REFRESH_INTERVAL {
            let target: NodeId = rand::random();
            let seeds = core.closest(&target, 2 * K);
            lookup(&core, target, Kind::FindNode, seeds, Instant::now() + BOOTSTRAP_TIME).await;
            last_refresh = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u8, port: u16) -> NodeInfo {
        let mut node_id = [0; 20];
        node_id[0] = id;
        NodeInfo { id: node_id, addr: SocketAddrV4::new([1, 1, 1, 1].into(), port) }
    }

    fn candidates() -> Candidates {
        Candidates { target: [0; 20], own: [0xff; 20], by_distance: BTreeMap::new(), seen: HashSet::new() }
    }

    #[test]
    fn asks_the_closest_first_and_converges() {
        let now = Instant::now();
        let mut c = candidates();
        for i in 1..=20u8 {
            c.add(node(i, u16::from(i)));
        }
        let first: Vec<u16> = (0..K).map(|_| c.next(now).unwrap().1.port()).collect();
        assert_eq!(first, (1..=K as u16).collect::<Vec<_>>());
        assert_eq!(c.active(now), K);
        assert_eq!(c.next(now), None, "only the K closest are asked while they are outstanding");

        // The closest fails: the next one moves into the top K.
        c.set(&distance(&node(1, 1).id, &[0; 20]), State::Failed);
        assert_eq!(c.next(now).unwrap().1.port(), K as u16 + 1);
        for i in 2..=K as u8 + 1 {
            c.set(&distance(&node(i, 0).id, &[0; 20]), State::Answered);
        }
        assert_eq!(c.next(now), None, "the K closest live nodes answered: converged");
    }

    #[test]
    fn slow_queries_give_up_their_place() {
        let now = Instant::now();
        let mut c = candidates();
        for i in 1..=20u8 {
            c.add(node(i, u16::from(i)));
        }
        for _ in 0..K {
            c.next(now).unwrap();
        }
        assert_eq!(c.next(now + SOFT_TIMEOUT / 2), None);
        let later = now + SOFT_TIMEOUT;
        assert_eq!(c.active(later), 0);
        assert_eq!(c.next(later).unwrap().1.port(), K as u16 + 1);
    }

    #[test]
    fn skips_our_own_id_and_repeated_addresses() {
        let mut c = candidates();
        c.add(NodeInfo { id: [0xff; 20], addr: SocketAddrV4::new([1, 1, 1, 1].into(), 1) });
        c.add(node(5, 2));
        c.add(node(6, 2));
        assert_eq!(c.by_distance.len(), 1);
    }

    #[test]
    fn caps_the_candidate_set() {
        let mut c = candidates();
        for i in 0..1000u16 {
            let mut id = [0; 20];
            id[..2].copy_from_slice(&(i + 1).to_be_bytes());
            c.add(NodeInfo { id, addr: SocketAddrV4::new([1, 1, 1, 1].into(), i + 1) });
        }
        assert_eq!(c.by_distance.len(), MAX_CANDIDATES);
        assert_eq!(c.next(Instant::now()).unwrap().1.port(), 1, "the farthest are dropped, not the closest");
    }
}

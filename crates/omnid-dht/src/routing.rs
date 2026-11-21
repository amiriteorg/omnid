//! The routing table: nodes that have answered us, in Kademlia buckets by
//! XOR distance from our ID. Addresses other nodes tell us about are only
//! lookup candidates until they answer a query themselves.

use std::net::SocketAddrV4;
use std::time::Instant;

use crate::krpc::{NodeId, NodeInfo};

/// Bucket size, and how many closest nodes a lookup converges on.
pub(crate) const K: usize = 8;
/// Consecutive unanswered queries before a node is dropped.
const MAX_FAILURES: u8 = 2;

#[derive(Debug, Clone)]
struct Entry {
    info: NodeInfo,
    last_seen: Instant,
    failures: u8,
}

pub(crate) struct RoutingTable {
    own: NodeId,
    buckets: Vec<Vec<Entry>>,
}

impl RoutingTable {
    pub(crate) fn new(own: NodeId) -> Self {
        Self { own, buckets: vec![Vec::new(); 160] }
    }

    pub(crate) fn len(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    /// Records a node that answered a query.
    pub(crate) fn heard_from(&mut self, info: NodeInfo, now: Instant) {
        let Some(index) = bucket_index(&self.own, &info.id) else {
            return;
        };
        // One entry per address: a node that restarts comes back with a new ID.
        for bucket in &mut self.buckets {
            bucket.retain(|e| e.info.addr != info.addr || e.info.id == info.id);
        }
        let bucket = &mut self.buckets[index];
        let fresh = Entry { info, last_seen: now, failures: 0 };
        if let Some(entry) = bucket.iter_mut().find(|e| e.info.id == info.id) {
            *entry = fresh;
        } else if bucket.len() < K {
            bucket.push(fresh);
        } else if let Some(worst) = bucket.iter_mut().filter(|e| e.failures > 0).max_by_key(|e| e.failures) {
            // A full bucket only takes a newcomer in place of a node that
            // stopped answering. Long-lived nodes are the likeliest to stay.
            *worst = fresh;
        }
    }

    /// Records a query to `addr` that went unanswered.
    pub(crate) fn failed(&mut self, addr: SocketAddrV4) {
        for bucket in &mut self.buckets {
            if let Some(i) = bucket.iter().position(|e| e.info.addr == addr) {
                bucket[i].failures = bucket[i].failures.saturating_add(1);
                if bucket[i].failures >= MAX_FAILURES {
                    bucket.remove(i);
                }
                return;
            }
        }
    }

    /// The `n` nodes closest to `target`, closest first.
    pub(crate) fn closest(&self, target: &NodeId, n: usize) -> Vec<NodeInfo> {
        let mut nodes: Vec<NodeInfo> = self.nodes().collect();
        nodes.sort_by_key(|node| distance(&node.id, target));
        nodes.truncate(n);
        nodes
    }

    /// Up to `n` nodes not heard from since `before`, oldest first.
    pub(crate) fn stale(&self, before: Instant, n: usize) -> Vec<NodeInfo> {
        let mut stale: Vec<&Entry> = self.buckets.iter().flatten().filter(|e| e.last_seen < before).collect();
        stale.sort_by_key(|e| e.last_seen);
        stale.into_iter().take(n).map(|e| e.info).collect()
    }

    pub(crate) fn nodes(&self) -> impl Iterator<Item = NodeInfo> + '_ {
        self.buckets.iter().flatten().map(|e| e.info)
    }
}

pub(crate) fn distance(a: &NodeId, b: &NodeId) -> NodeId {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// The number of leading bits `id` shares with our own ID, or `None` for
/// our own ID.
fn bucket_index(own: &NodeId, id: &NodeId) -> Option<usize> {
    let d = distance(own, id);
    let byte = d.iter().position(|b| *b != 0)?;
    Some(byte * 8 + d[byte].leading_zeros() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: NodeId, port: u16) -> NodeInfo {
        NodeInfo { id, addr: SocketAddrV4::new([1, 2, 3, 4].into(), port) }
    }

    /// An ID sharing exactly `prefix` leading bits with the all-zero ID.
    fn id_at(prefix: usize, tail: u8) -> NodeId {
        let mut id = [0; 20];
        id[prefix / 8] = 0x80 >> (prefix % 8);
        id[19] |= tail;
        id
    }

    #[test]
    fn buckets_by_shared_prefix() {
        let own = [0; 20];
        assert_eq!(bucket_index(&own, &own), None);
        assert_eq!(bucket_index(&own, &[0xff; 20]), Some(0));
        assert_eq!(bucket_index(&own, &id_at(9, 0)), Some(9));
        assert_eq!(bucket_index(&own, &id_at(159, 0)), Some(159));
    }

    #[test]
    fn full_buckets_keep_old_nodes_until_they_fail() {
        let now = Instant::now();
        let mut table = RoutingTable::new([0; 20]);
        for i in 0..K as u8 {
            table.heard_from(node(id_at(0, i + 1), 1000 + u16::from(i)), now);
        }
        assert_eq!(table.len(), K);
        let newcomer = node(id_at(0, 100), 2000);
        table.heard_from(newcomer, now);
        assert!(!table.nodes().any(|n| n == newcomer), "a full bucket does not evict a live node");

        table.failed(SocketAddrV4::new([1, 2, 3, 4].into(), 1000));
        table.heard_from(newcomer, now);
        assert!(table.nodes().any(|n| n == newcomer), "a failing node makes room");
        assert_eq!(table.len(), K);

        table.heard_from(node(id_at(5, 1), 3000), now);
        assert_eq!(table.len(), K + 1, "other buckets are independent");
    }

    #[test]
    fn drops_nodes_after_repeated_failures() {
        let mut table = RoutingTable::new([0; 20]);
        let n = node(id_at(3, 0), 6881);
        table.heard_from(n, Instant::now());
        table.failed(n.addr);
        assert_eq!(table.len(), 1);
        table.failed(n.addr);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn an_address_keeps_one_entry_when_its_id_changes() {
        let mut table = RoutingTable::new([0; 20]);
        table.heard_from(node(id_at(1, 0), 6881), Instant::now());
        table.heard_from(node(id_at(40, 0), 6881), Instant::now());
        assert_eq!(table.nodes().collect::<Vec<_>>(), vec![node(id_at(40, 0), 6881)]);
        table.heard_from(node([0; 20], 9), Instant::now());
        assert_eq!(table.len(), 1, "our own ID is never stored");
    }

    #[test]
    fn closest_sorts_by_xor_distance() {
        let mut table = RoutingTable::new([0; 20]);
        for (i, prefix) in [0, 4, 8, 12, 16].into_iter().enumerate() {
            table.heard_from(node(id_at(prefix, 0), i as u16 + 1), Instant::now());
        }
        let target = id_at(12, 1);
        let closest: Vec<NodeId> = table.closest(&target, 3).into_iter().map(|n| n.id).collect();
        assert_eq!(closest, vec![id_at(12, 0), id_at(16, 0), id_at(8, 0)]);
    }

    #[test]
    fn stale_lists_the_oldest_first() {
        let start = Instant::now();
        let mut table = RoutingTable::new([0; 20]);
        let (a, b, c) = (node(id_at(1, 0), 1), node(id_at(2, 0), 2), node(id_at(3, 0), 3));
        table.heard_from(b, start + std::time::Duration::from_secs(2));
        table.heard_from(a, start + std::time::Duration::from_secs(1));
        table.heard_from(c, start + std::time::Duration::from_secs(10));
        assert_eq!(table.stale(start + std::time::Duration::from_secs(5), 8), vec![a, b]);
        assert_eq!(table.stale(start + std::time::Duration::from_secs(5), 1), vec![a]);
    }
}

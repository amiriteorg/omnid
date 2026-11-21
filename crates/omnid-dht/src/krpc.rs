//! KRPC, the DHT's message format (BEP 5): one bencoded dictionary per UDP
//! datagram, matched to its query by the transaction ID `t`.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::bencode::{self, DecodeError, Limits, Value};

pub(crate) type NodeId = [u8; 20];

/// A node's ID and address, as carried in compact node info (26 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodeInfo {
    pub id: NodeId,
    pub addr: SocketAddrV4,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Query {
    Ping,
    FindNode {
        target: NodeId,
    },
    GetPeers {
        info_hash: [u8; 20],
    },
    /// BEP 51.
    SampleInfohashes {
        target: NodeId,
    },
}

impl Query {
    pub(crate) fn method(&self) -> &'static str {
        match self {
            Query::Ping => "ping",
            Query::FindNode { .. } => "find_node",
            Query::GetPeers { .. } => "get_peers",
            Query::SampleInfohashes { .. } => "sample_infohashes",
        }
    }
}

/// Encodes a query from `id`. Every query carries `ro: 1` (BEP 43), so the
/// nodes asked keep us out of their routing tables and never query us.
pub(crate) fn encode_query(tid: &[u8], id: &NodeId, query: &Query) -> Vec<u8> {
    let mut args = vec![("id", Value::Bytes(id))];
    match query {
        Query::Ping => {}
        Query::FindNode { target } | Query::SampleInfohashes { target } => {
            args.push(("target", Value::Bytes(target)));
        }
        Query::GetPeers { info_hash } => args.push(("info_hash", Value::Bytes(info_hash))),
    }
    bencode::encode(&Value::dict(vec![
        ("a", Value::dict(args)),
        ("q", Value::Bytes(query.method().as_bytes())),
        ("ro", Value::Int(1)),
        ("t", Value::Bytes(tid)),
        ("y", Value::Bytes(b"q")),
    ]))
}

/// A node's answer. Responses do not say which query they answer, so this
/// holds every field the queries we send can return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Response {
    pub id: NodeId,
    /// IPv4 compact node info. `nodes6` (BEP 32) is ignored.
    pub nodes: Vec<NodeInfo>,
    /// get_peers: peers in the swarm. IPv6 entries are skipped.
    pub values: Vec<SocketAddrV4>,
    /// sample_infohashes: info hashes the node stores.
    pub samples: Vec<[u8; 20]>,
    /// sample_infohashes: seconds before the node wants to be sampled again.
    pub interval: Option<u64>,
    /// sample_infohashes: how many info hashes the node stores in all.
    pub num: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Message {
    /// Someone queried us. A read-only node does not answer, so only the
    /// method is kept, for logging.
    Query {
        method: Vec<u8>,
    },
    Response {
        tid: Vec<u8>,
        response: Response,
    },
    Error {
        tid: Vec<u8>,
        code: i64,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum KrpcError {
    #[error(transparent)]
    Bencode(#[from] DecodeError),
    #[error("malformed KRPC message: {0}")]
    Malformed(&'static str),
}

/// Error messages come from the network and end up in logs.
const MAX_ERROR_MESSAGE: usize = 200;

pub(crate) fn decode(packet: &[u8]) -> Result<Message, KrpcError> {
    let msg = bencode::decode(packet, Limits::KRPC)?;
    let tid = msg.get(b"t").and_then(Value::as_bytes).ok_or(KrpcError::Malformed("no transaction ID"))?;
    match msg.get(b"y").and_then(Value::as_bytes) {
        Some(b"q") => {
            let method = msg.get(b"q").and_then(Value::as_bytes).ok_or(KrpcError::Malformed("no method"))?;
            Ok(Message::Query { method: method.to_vec() })
        }
        Some(b"r") => {
            let body = msg.get(b"r").ok_or(KrpcError::Malformed("no response body"))?;
            Ok(Message::Response { tid: tid.to_vec(), response: parse_response(body)? })
        }
        Some(b"e") => {
            let error = msg.get(b"e").and_then(Value::as_list).unwrap_or_default();
            let code = error.first().and_then(Value::as_int).unwrap_or_default();
            let message = error.get(1).and_then(Value::as_bytes).unwrap_or_default();
            let message: String = String::from_utf8_lossy(message).chars().take(MAX_ERROR_MESSAGE).collect();
            Ok(Message::Error { tid: tid.to_vec(), code, message })
        }
        _ => Err(KrpcError::Malformed("unknown message type")),
    }
}

fn parse_response(body: &Value<'_>) -> Result<Response, KrpcError> {
    let id = body
        .get(b"id")
        .and_then(Value::as_bytes)
        .and_then(|id| id.try_into().ok())
        .ok_or(KrpcError::Malformed("no node ID"))?;
    let nodes = body.get(b"nodes").and_then(Value::as_bytes).map(parse_nodes).unwrap_or_default();
    let values = body
        .get(b"values")
        .and_then(Value::as_list)
        .map(|values| values.iter().filter_map(|v| v.as_bytes().and_then(parse_peer)).collect())
        .unwrap_or_default();
    let samples =
        body.get(b"samples").and_then(Value::as_bytes).map(|s| s.as_chunks::<20>().0.to_vec()).unwrap_or_default();
    let count = |key: &[u8]| body.get(key).and_then(Value::as_int).and_then(|n| u64::try_from(n).ok());
    Ok(Response { id, nodes, values, samples, interval: count(b"interval"), num: count(b"num") })
}

/// Compact node info: 20-byte ID, 4-byte IPv4, 2-byte port, back to back. A
/// trailing partial entry is ignored.
pub(crate) fn parse_nodes(compact: &[u8]) -> Vec<NodeInfo> {
    compact
        .as_chunks::<26>()
        .0
        .iter()
        .filter_map(|c| {
            let id = c[..20].try_into().ok()?;
            Some(NodeInfo { id, addr: parse_peer(&c[20..])? })
        })
        .collect()
}

/// Compact peer info: 4-byte IPv4 and 2-byte port.
pub(crate) fn parse_peer(compact: &[u8]) -> Option<SocketAddrV4> {
    let [a, b, c, d, hi, lo] = compact.try_into().ok()?;
    Some(SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), u16::from_be_bytes([hi, lo])))
}

/// The other side of the protocol, for tests that stand in for DHT nodes.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) struct IncomingQuery {
        pub tid: Vec<u8>,
        pub query: Query,
        pub read_only: bool,
    }

    pub(crate) fn decode_query(packet: &[u8]) -> Option<IncomingQuery> {
        let msg = bencode::decode(packet, Limits::KRPC).ok()?;
        if msg.get(b"y")?.as_bytes()? != b"q" {
            return None;
        }
        let args = msg.get(b"a")?;
        let arg = |key: &[u8]| -> Option<[u8; 20]> { args.get(key)?.as_bytes()?.try_into().ok() };
        let query = match msg.get(b"q")?.as_bytes()? {
            b"ping" => Query::Ping,
            b"find_node" => Query::FindNode { target: arg(b"target")? },
            b"get_peers" => Query::GetPeers { info_hash: arg(b"info_hash")? },
            b"sample_infohashes" => Query::SampleInfohashes { target: arg(b"target")? },
            _ => return None,
        };
        arg(b"id")?;
        Some(IncomingQuery {
            tid: msg.get(b"t")?.as_bytes()?.to_vec(),
            query,
            read_only: msg.get(b"ro").and_then(Value::as_int) == Some(1),
        })
    }

    pub(crate) fn compact_peer(addr: SocketAddrV4) -> [u8; 6] {
        let [a, b, c, d] = addr.ip().octets();
        let [hi, lo] = addr.port().to_be_bytes();
        [a, b, c, d, hi, lo]
    }

    pub(crate) fn compact_nodes(nodes: &[NodeInfo]) -> Vec<u8> {
        nodes.iter().flat_map(|n| n.id.into_iter().chain(compact_peer(n.addr))).collect()
    }

    pub(crate) fn encode_response(tid: &[u8], r: &Response) -> Vec<u8> {
        let nodes = compact_nodes(&r.nodes);
        let values: Vec<[u8; 6]> = r.values.iter().map(|p| compact_peer(*p)).collect();
        let samples: Vec<u8> = r.samples.concat();
        let mut body = vec![("id", Value::Bytes(&r.id))];
        if !r.nodes.is_empty() {
            body.push(("nodes", Value::Bytes(&nodes)));
        }
        if !r.values.is_empty() {
            body.push(("values", Value::List(values.iter().map(|v| Value::Bytes(v)).collect())));
        }
        if !r.samples.is_empty() {
            body.push(("samples", Value::Bytes(&samples)));
        }
        if let Some(interval) = r.interval {
            body.push(("interval", Value::Int(interval as i64)));
        }
        if let Some(num) = r.num {
            body.push(("num", Value::Int(num as i64)));
        }
        bencode::encode(&Value::dict(vec![
            ("r", Value::dict(body)),
            ("t", Value::Bytes(tid)),
            ("y", Value::Bytes(b"r")),
        ]))
    }

    pub(crate) fn encode_error(tid: &[u8], code: i64, message: &str) -> Vec<u8> {
        bencode::encode(&Value::dict(vec![
            ("e", Value::List(vec![Value::Int(code), Value::Bytes(message.as_bytes())])),
            ("t", Value::Bytes(tid)),
            ("y", Value::Bytes(b"e")),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const ID: &NodeId = b"abcdefghij0123456789";
    const TARGET: &NodeId = b"mnopqrstuvwxyz123456";

    fn addr(s: &str) -> SocketAddrV4 {
        s.parse().unwrap()
    }

    #[test]
    fn encodes_queries_read_only() {
        // BEP 5's examples, plus BEP 43's `ro`.
        assert_eq!(
            encode_query(b"aa", ID, &Query::Ping),
            b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping2:roi1e1:t2:aa1:y1:qe".to_vec()
        );
        assert_eq!(
            encode_query(b"aa", ID, &Query::FindNode { target: *TARGET }),
            b"d1:ad2:id20:abcdefghij01234567896:target20:mnopqrstuvwxyz123456e1:q9:find_node2:roi1e1:t2:aa1:y1:qe"
                .to_vec()
        );
        assert_eq!(
            encode_query(b"aa", ID, &Query::GetPeers { info_hash: *TARGET }),
            b"d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz123456e1:q9:get_peers2:roi1e1:t2:aa1:y1:qe"
                .to_vec()
        );
        assert_eq!(
            encode_query(b"aa", ID, &Query::SampleInfohashes { target: *TARGET }),
            b"d1:ad2:id20:abcdefghij01234567896:target20:mnopqrstuvwxyz123456e1:q17:sample_infohashes2:roi1e1:t2:aa1:y1:qe"
                .to_vec()
        );
    }

    #[test]
    fn queries_round_trip() {
        for query in [
            Query::Ping,
            Query::FindNode { target: *TARGET },
            Query::GetPeers { info_hash: *TARGET },
            Query::SampleInfohashes { target: *TARGET },
        ] {
            let packet = encode_query(b"t1", ID, &query);
            let incoming = decode_query(&packet).unwrap();
            assert_eq!((incoming.tid.as_slice(), &incoming.query, incoming.read_only), (&b"t1"[..], &query, true));
            assert_eq!(decode(&packet).unwrap(), Message::Query { method: query.method().as_bytes().to_vec() });
        }
    }

    #[test]
    fn decodes_ping_response() {
        let msg = decode(b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re").unwrap();
        assert_eq!(
            msg,
            Message::Response { tid: b"aa".to_vec(), response: Response { id: *TARGET, ..Default::default() } }
        );
    }

    #[test]
    fn decodes_get_peers_with_values() {
        let msg = decode(b"d1:rd2:id20:abcdefghij01234567895:token8:aoeusnth6:valuesl6:axje.u6:idhtnmee1:t2:aa1:y1:re")
            .unwrap();
        let Message::Response { response, .. } = msg else { panic!("not a response") };
        assert_eq!(response.values, vec![addr("97.120.106.101:11893"), addr("105.100.104.116:28269")]);
        assert!(response.nodes.is_empty());
    }

    #[test]
    fn decodes_get_peers_with_nodes() {
        let nodes = vec![
            NodeInfo { id: [1; 20], addr: addr("1.2.3.4:6881") },
            NodeInfo { id: [2; 20], addr: addr("5.6.7.8:51413") },
        ];
        let mut compact = compact_nodes(&nodes);
        assert_eq!(compact.len(), 52);
        compact.extend_from_slice(&[9; 10]); // trailing partial entry
        let mut packet = b"d1:rd2:id20:abcdefghij01234567895:nodes".to_vec();
        packet.extend_from_slice(format!("{}:", compact.len()).as_bytes());
        packet.extend_from_slice(&compact);
        // One IPv4 peer and one 18-byte IPv6 peer, which is skipped.
        packet.extend_from_slice(b"5:token8:aoeusnth6:valuesl6:axje.u18:000000000000000000ee1:t2:aa1:y1:re");
        let Message::Response { response, .. } = decode(&packet).unwrap() else { panic!() };
        assert_eq!(response.nodes, nodes);
        assert_eq!(response.values, vec![addr("97.120.106.101:11893")]);
    }

    #[test]
    fn decodes_sample_infohashes_response() {
        let response = Response {
            id: *ID,
            nodes: vec![NodeInfo { id: [3; 20], addr: addr("9.9.9.9:1234") }],
            samples: vec![[0xaa; 20], [0xbb; 20], [0xcc; 20]],
            interval: Some(21600),
            num: Some(300),
            ..Default::default()
        };
        let packet = encode_response(b"xy", &response);
        assert_eq!(decode(&packet).unwrap(), Message::Response { tid: b"xy".to_vec(), response });
    }

    #[test]
    fn decodes_errors() {
        let msg = decode(b"d1:eli201e23:A Generic Error Occurede1:t2:aa1:y1:ee").unwrap();
        assert_eq!(msg, Message::Error { tid: b"aa".to_vec(), code: 201, message: "A Generic Error Occured".into() });
        let long = encode_error(b"aa", 204, &"x".repeat(10_000));
        let Message::Error { message, .. } = decode(&long).unwrap() else { panic!() };
        assert_eq!(message.len(), MAX_ERROR_MESSAGE);
        // Missing pieces of the error list are tolerated.
        assert!(matches!(decode(b"d1:ele1:t2:aa1:y1:ee").unwrap(), Message::Error { code: 0, .. }));
    }

    #[test]
    fn rejects_malformed_messages() {
        for packet in [
            &b""[..],
            b"garbage",
            b"le",
            b"d1:y1:re",
            b"d1:t2:aa1:y1:ze",
            b"d1:t2:aa1:y1:re",
            b"d1:rde1:t2:aa1:y1:re",
            b"d1:rd2:id3:abce1:t2:aa1:y1:re",
            b"d1:rd2:idi5ee1:t2:aa1:y1:re",
            b"d1:t2:aa1:y1:qe",
        ] {
            assert!(decode(packet).is_err(), "{:?} should not decode", String::from_utf8_lossy(packet));
        }
    }

    #[test]
    fn samples_must_be_whole_hashes() {
        let mut packet = b"d1:rd2:id20:abcdefghij01234567897:samples30:".to_vec();
        packet.extend_from_slice(&[7; 30]);
        packet.extend_from_slice(b"e1:t2:aa1:y1:re");
        let Message::Response { response, .. } = decode(&packet).unwrap() else { panic!() };
        assert_eq!(response.samples, vec![[7; 20]]);
    }
}

//! The peer wire, only as far as BEP 9 needs: the BEP 3 handshake with the
//! extension bit, the BEP 10 extended handshake and `ut_metadata` requests.
//! Nothing here sends `interested` or `request`, so no piece data is ever
//! asked for, and nothing is served.

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::bencode::{self, DecodeError, Limits, Value};

const PROTOCOL: &[u8; 20] = b"\x13BitTorrent protocol";
/// Reserved byte 5, bit 0x10: the extension protocol (BEP 10).
const EXTENSION_BYTE: usize = 20 + 5;
const EXTENSION_BIT: u8 = 0x10;
const MSG_EXTENDED: u8 = 20;
const EXT_HANDSHAKE: u8 = 0;
/// The ID peers put on the ut_metadata messages they send us.
const UT_METADATA: u8 = 1;
/// Metadata travels in 16 KiB pieces (BEP 9).
pub(crate) const PIECE: usize = 16 * 1024;
/// Longer messages are refused. A metadata piece is 16 KiB and a bitfield
/// for even a huge torrent is a few tens of KiB.
const MAX_MESSAGE: usize = 1 << 20;
const MAX_EXTENDED: usize = 64 * 1024;
/// Metadata requests in flight per peer.
const WINDOW: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT: &str = concat!("omnid ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub(crate) enum PeerError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("timed out")]
    Timeout,
    #[error("peer is serving a different torrent")]
    WrongTorrent,
    #[error("peer does not speak the extension protocol")]
    NoExtensions,
    #[error("peer does not offer the metadata")]
    NoMetadata,
    #[error("metadata is {0} bytes, over the limit")]
    TooLarge(usize),
    #[error("peer rejected metadata piece {0}")]
    Rejected(u32),
    #[error("malformed message: {0}")]
    Malformed(#[from] DecodeError),
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    #[error("metadata does not match the info hash")]
    HashMismatch,
}

/// An Azureus-style peer ID: client `OM`, version 0.1.0, then random.
pub(crate) fn peer_id() -> [u8; 20] {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut id = *b"-OM0100-000000000000";
    let random: [u8; 12] = rand::random();
    for (slot, r) in id[8..].iter_mut().zip(random) {
        *slot = ALPHABET[usize::from(r) % ALPHABET.len()];
    }
    id
}

/// Connects to a peer and fetches the info dict for `info_hash`, verified.
pub(crate) async fn fetch_metadata(
    addr: SocketAddr,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    max_size: usize,
) -> Result<Vec<u8>, PeerError> {
    let stream =
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await.map_err(|_| PeerError::Timeout)??;
    let _ = stream.set_nodelay(true);
    exchange(stream, info_hash, peer_id, max_size).await
}

/// The metadata exchange over any stream. The stream is dropped, and the
/// connection closed, as soon as the metadata is complete.
pub(crate) async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    max_size: usize,
) -> Result<Vec<u8>, PeerError> {
    stream.write_all(&handshake(info_hash, peer_id)).await?;
    let mut theirs = [0; 68];
    timed(stream.read_exact(&mut theirs)).await?;
    let theirs = parse_handshake(&theirs)?;
    if theirs.info_hash != *info_hash {
        return Err(PeerError::WrongTorrent);
    }
    if !theirs.extensions {
        return Err(PeerError::NoExtensions);
    }
    stream.write_all(&extended_handshake()).await?;

    let mut buf = Vec::new();
    let (their_id, mut pieces) = loop {
        let (ext, body) = next_extended(&mut stream, &mut buf).await?;
        if ext == EXT_HANDSHAKE {
            let hs = parse_extended_handshake(body)?;
            let (Some(id), Some(size)) = (hs.ut_metadata, hs.metadata_size) else {
                return Err(PeerError::NoMetadata);
            };
            break (id, Assembler::new(size, max_size)?);
        }
    };

    let (mut next, mut in_flight) = (0, 0);
    while !pieces.is_complete() {
        while in_flight < WINDOW && next < pieces.count() {
            stream.write_all(&metadata_request(their_id, next as u32)).await?;
            next += 1;
            in_flight += 1;
        }
        let (ext, body) = next_extended(&mut stream, &mut buf).await?;
        if ext != UT_METADATA {
            continue;
        }
        match parse_metadata_message(body)? {
            MetadataMessage::Data { piece, total_size, data } => {
                if total_size.is_some_and(|size| size != pieces.size as u64) {
                    return Err(PeerError::Protocol("total_size disagrees with metadata_size"));
                }
                pieces.add(piece, data)?;
                in_flight = in_flight.saturating_sub(1);
            }
            MetadataMessage::Reject { piece } => return Err(PeerError::Rejected(piece)),
            // We have nothing to serve, and a read-only client says nothing.
            MetadataMessage::Request { .. } => {}
        }
    }
    pieces.finish(info_hash)
}

pub(crate) fn handshake(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> [u8; 68] {
    let mut out = [0; 68];
    out[..20].copy_from_slice(PROTOCOL);
    out[EXTENSION_BYTE] = EXTENSION_BIT;
    out[28..48].copy_from_slice(info_hash);
    out[48..].copy_from_slice(peer_id);
    out
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Handshake {
    pub extensions: bool,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

pub(crate) fn parse_handshake(buf: &[u8; 68]) -> Result<Handshake, PeerError> {
    if &buf[..20] != PROTOCOL {
        return Err(PeerError::Protocol("not a BitTorrent handshake"));
    }
    let (mut info_hash, mut peer_id) = ([0; 20], [0; 20]);
    info_hash.copy_from_slice(&buf[28..48]);
    peer_id.copy_from_slice(&buf[48..]);
    Ok(Handshake { extensions: buf[EXTENSION_BYTE] & EXTENSION_BIT != 0, info_hash, peer_id })
}

/// A BEP 10 message: length, message ID 20, extension ID, payload.
fn extended(ext: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 6);
    out.extend_from_slice(&(payload.len() as u32 + 2).to_be_bytes());
    out.extend_from_slice(&[MSG_EXTENDED, ext]);
    out.extend_from_slice(payload);
    out
}

/// Offers ut_metadata and nothing else. No listen port, no metadata size:
/// we have nothing to give.
fn extended_handshake() -> Vec<u8> {
    let payload = bencode::encode(&Value::dict(vec![
        ("m", Value::dict(vec![("ut_metadata", Value::Int(UT_METADATA.into()))])),
        ("v", Value::Bytes(CLIENT.as_bytes())),
    ]));
    extended(EXT_HANDSHAKE, &payload)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExtendedHandshake {
    /// The ID to put on ut_metadata messages to this peer.
    pub ut_metadata: Option<u8>,
    pub metadata_size: Option<u64>,
}

pub(crate) fn parse_extended_handshake(payload: &[u8]) -> Result<ExtendedHandshake, PeerError> {
    let dict = bencode::decode(payload, Limits::PEER)?;
    let ut_metadata = dict
        .get(b"m")
        .and_then(|m| m.get(b"ut_metadata"))
        .and_then(Value::as_int)
        .and_then(|id| u8::try_from(id).ok())
        .filter(|id| *id != 0);
    let metadata_size = dict.get(b"metadata_size").and_then(Value::as_int).and_then(|n| u64::try_from(n).ok());
    Ok(ExtendedHandshake { ut_metadata, metadata_size })
}

pub(crate) fn metadata_request(their_id: u8, piece: u32) -> Vec<u8> {
    let payload = bencode::encode(&Value::dict(vec![("msg_type", Value::Int(0)), ("piece", Value::Int(piece.into()))]));
    extended(their_id, &payload)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MetadataMessage<'a> {
    Request { piece: u32 },
    Data { piece: u32, total_size: Option<u64>, data: &'a [u8] },
    Reject { piece: u32 },
}

/// A ut_metadata message: a bencoded header, then the piece for `Data`.
pub(crate) fn parse_metadata_message(payload: &[u8]) -> Result<MetadataMessage<'_>, PeerError> {
    let (header, used) = bencode::decode_prefix(payload, Limits::PEER)?;
    let piece = header
        .get(b"piece")
        .and_then(Value::as_int)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or(PeerError::Protocol("ut_metadata message without a piece"))?;
    match header.get(b"msg_type").and_then(Value::as_int) {
        Some(0) => Ok(MetadataMessage::Request { piece }),
        Some(1) => {
            let total_size = header.get(b"total_size").and_then(Value::as_int).and_then(|n| u64::try_from(n).ok());
            Ok(MetadataMessage::Data { piece, total_size, data: &payload[used..] })
        }
        Some(2) => Ok(MetadataMessage::Reject { piece }),
        _ => Err(PeerError::Protocol("unknown ut_metadata message type")),
    }
}

/// Collects metadata pieces from one peer. The advertised size is checked
/// against the limit before anything is stored, and pieces take memory only
/// as they arrive.
pub(crate) struct Assembler {
    size: usize,
    pieces: BTreeMap<u32, Vec<u8>>,
}

impl Assembler {
    pub(crate) fn new(size: u64, max: usize) -> Result<Self, PeerError> {
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        if size > max {
            return Err(PeerError::TooLarge(size));
        }
        if size == 0 {
            return Err(PeerError::Protocol("metadata_size is zero"));
        }
        Ok(Self { size, pieces: BTreeMap::new() })
    }

    pub(crate) fn count(&self) -> usize {
        self.size.div_ceil(PIECE)
    }

    /// Every piece is 16 KiB except the last, which holds the rest.
    pub(crate) fn add(&mut self, piece: u32, data: &[u8]) -> Result<(), PeerError> {
        let index = piece as usize;
        if index >= self.count() {
            return Err(PeerError::Protocol("metadata piece out of range"));
        }
        let expected = if index + 1 == self.count() { self.size - index * PIECE } else { PIECE };
        if data.len() != expected {
            return Err(PeerError::Protocol("metadata piece has the wrong length"));
        }
        self.pieces.entry(piece).or_insert_with(|| data.to_vec());
        Ok(())
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.pieces.len() == self.count()
    }

    /// Joins the pieces and checks the result against the info hash.
    pub(crate) fn finish(self, info_hash: &[u8; 20]) -> Result<Vec<u8>, PeerError> {
        if !self.is_complete() {
            return Err(PeerError::Protocol("metadata is incomplete"));
        }
        let mut info = Vec::with_capacity(self.size);
        for piece in self.pieces.into_values() {
            info.extend_from_slice(&piece);
        }
        if crate::meta::info_hash(&info) != *info_hash {
            return Err(PeerError::HashMismatch);
        }
        Ok(info)
    }
}

/// Reads messages until an extended one arrives, skipping the rest unread
/// into memory: bitfields, haves, chokes.
async fn next_extended<'b, S: AsyncRead + Unpin>(
    stream: &mut S,
    buf: &'b mut Vec<u8>,
) -> Result<(u8, &'b [u8]), PeerError> {
    loop {
        let mut len = [0; 4];
        timed(stream.read_exact(&mut len)).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 {
            continue; // keep-alive
        }
        if len > MAX_MESSAGE {
            return Err(PeerError::Protocol("message too long"));
        }
        let mut id = [0; 1];
        timed(stream.read_exact(&mut id)).await?;
        let body = len - 1;
        if id[0] != MSG_EXTENDED {
            let skipped = timed(tokio::io::copy(&mut (&mut *stream).take(body as u64), &mut tokio::io::sink())).await?;
            if skipped < body as u64 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            continue;
        }
        if body == 0 || body > MAX_EXTENDED {
            return Err(PeerError::Protocol("bad extended message length"));
        }
        buf.resize(body, 0);
        timed(stream.read_exact(buf)).await?;
        return Ok((buf[0], &buf[1..]));
    }
}

async fn timed<T>(io: impl Future<Output = io::Result<T>>) -> Result<T, PeerError> {
    tokio::time::timeout(READ_TIMEOUT, io).await.map_err(|_| PeerError::Timeout)?.map_err(PeerError::from)
}

/// A peer that serves metadata, for tests. It records the IDs of the
/// non-extended messages the client sends, which must stay empty.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Behavior {
        Good,
        Corrupt,
        Reject,
        WrongTorrent,
        NoExtensions,
        Oversize(u64),
        /// Says nothing after the client's handshake, and hangs up after
        /// this long or when the client does.
        Stall(Duration),
        /// Answers the handshake with bytes that are not one.
        Garbage,
    }

    /// The ID the fake peer asks for on ut_metadata messages.
    const FAKE_UT_METADATA: u8 = 3;

    pub(crate) async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
        mut stream: S,
        info_hash: [u8; 20],
        info: Vec<u8>,
        behavior: Behavior,
    ) -> io::Result<Vec<u8>> {
        let mut theirs = [0; 68];
        stream.read_exact(&mut theirs).await?;
        let theirs = parse_handshake(&theirs).expect("client sent a handshake");
        assert!(theirs.extensions && theirs.info_hash == info_hash, "client handshake: {theirs:?}");
        match behavior {
            Behavior::Stall(limit) => {
                let _ = tokio::time::timeout(limit, stream.read_to_end(&mut Vec::new())).await;
                return Ok(Vec::new());
            }
            Behavior::Garbage => {
                stream.write_all(&[0xab; 68]).await?;
                let _ = stream.read_to_end(&mut Vec::new()).await;
                return Ok(Vec::new());
            }
            _ => {}
        }

        let served_hash = if behavior == Behavior::WrongTorrent { [0xee; 20] } else { info_hash };
        let mut reply = handshake(&served_hash, b"-XX0000-fakepeer0001");
        if behavior == Behavior::NoExtensions {
            reply[EXTENSION_BYTE] = 0;
        }
        stream.write_all(&reply).await?;
        let mut forbidden = Vec::new();
        if matches!(behavior, Behavior::WrongTorrent | Behavior::NoExtensions) {
            let _ = stream.read_to_end(&mut Vec::new()).await;
            return Ok(forbidden);
        }
        // A bitfield and a keep-alive, which the client has to skip.
        stream.write_all(&[0, 0, 0, 3, 5, 0xff, 0x80, 0, 0, 0, 0]).await?;
        let size = match behavior {
            Behavior::Oversize(size) => size,
            _ => info.len() as u64,
        };
        let hs = bencode::encode(&Value::dict(vec![
            ("m", Value::dict(vec![("ut_metadata", Value::Int(FAKE_UT_METADATA.into()))])),
            ("metadata_size", Value::Int(size as i64)),
            ("v", Value::Bytes(b"fake")),
        ]));
        stream.write_all(&extended(EXT_HANDSHAKE, &hs)).await?;

        let mut client_ut = None;
        loop {
            let mut len = [0; 4];
            if stream.read_exact(&mut len).await.is_err() {
                return Ok(forbidden); // the client hung up
            }
            let mut msg = vec![0; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut msg).await?;
            match msg.split_first() {
                None => {}
                Some((&MSG_EXTENDED, rest)) => match rest.split_first() {
                    Some((&EXT_HANDSHAKE, body)) => client_ut = parse_extended_handshake(body).unwrap().ut_metadata,
                    Some((&FAKE_UT_METADATA, body)) => {
                        let MetadataMessage::Request { piece } = parse_metadata_message(body).unwrap() else {
                            panic!("client sent something other than a request");
                        };
                        let client_ut = client_ut.expect("extended handshake before requests");
                        let start = piece as usize * PIECE;
                        let mut data = info[start..(start + PIECE).min(info.len())].to_vec();
                        let header = match behavior {
                            Behavior::Reject => {
                                Value::dict(vec![("msg_type", Value::Int(2)), ("piece", Value::Int(piece.into()))])
                            }
                            _ => Value::dict(vec![
                                ("msg_type", Value::Int(1)),
                                ("piece", Value::Int(piece.into())),
                                ("total_size", Value::Int(info.len() as i64)),
                            ]),
                        };
                        if behavior == Behavior::Corrupt {
                            data[0] ^= 1;
                        }
                        let mut payload = bencode::encode(&header);
                        if behavior != Behavior::Reject {
                            payload.extend_from_slice(&data);
                        }
                        stream.write_all(&extended(client_ut, &payload)).await?;
                    }
                    other => panic!("unexpected extension message {other:?}"),
                },
                Some((&id, _)) => forbidden.push(id),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Behavior, serve};
    use super::*;

    /// An info dict over three pieces.
    fn fixture() -> (Vec<u8>, [u8; 20]) {
        let pieces = vec![0xab; 2 * PIECE + 100];
        let info = bencode::encode(&Value::dict(vec![
            ("length", Value::Int(12345)),
            ("name", Value::Bytes(b"fixture.bin")),
            ("piece length", Value::Int(16384)),
            ("pieces", Value::Bytes(&pieces)),
        ]));
        let hash = crate::meta::info_hash(&info);
        (info, hash)
    }

    async fn run(behavior: Behavior) -> (Result<Vec<u8>, PeerError>, Vec<u8>) {
        let (info, hash) = fixture();
        let (client, server) = tokio::io::duplex(8 * 1024);
        let peer = tokio::spawn(serve(server, hash, info, behavior));
        let result = exchange(client, &hash, &peer_id(), 10 << 20).await;
        let forbidden = peer.await.unwrap().unwrap();
        (result, forbidden)
    }

    #[test]
    fn parses_a_real_handshake() {
        let mut fixture = Vec::from(&b"\x13BitTorrent protocol"[..]);
        fixture.extend_from_slice(&[0, 0, 0, 0, 0, 0x10, 0, 0x05]); // extensions, fast, DHT
        fixture.extend_from_slice(&[0x08; 20]);
        fixture.extend_from_slice(b"-qB4500-k8Vw0aZ2xY3q");
        let hs = parse_handshake(&fixture.try_into().unwrap()).unwrap();
        assert_eq!(hs, Handshake { extensions: true, info_hash: [0x08; 20], peer_id: *b"-qB4500-k8Vw0aZ2xY3q" });

        let ours = handshake(&[1; 20], &[2; 20]);
        assert_eq!(&ours[..20], PROTOCOL);
        assert_eq!(&ours[20..28], &[0, 0, 0, 0, 0, 0x10, 0, 0]);
        assert!(parse_handshake(&[0; 68]).is_err());
    }

    #[test]
    fn parses_extended_handshakes() {
        // As qBittorrent (libtorrent) sends it.
        let libtorrent = b"d1:md11:lt_donthavei7e10:share_modei8e11:upload_onlyi3e12:ut_holepunchi4e\
            11:ut_metadatai2e6:ut_pexi1ee13:metadata_sizei5234e4:reqqi500e11:upload_onlyi0e\
            1:v17:qBittorrent/4.5.06:yourip4:\x7f\x00\x00\x01e";
        assert_eq!(
            parse_extended_handshake(libtorrent).unwrap(),
            ExtendedHandshake { ut_metadata: Some(2), metadata_size: Some(5234) }
        );
        // A peer that has no metadata yet, and one that disabled ut_metadata.
        let none = parse_extended_handshake(b"d1:md11:ut_metadatai3eee").unwrap();
        assert_eq!(none, ExtendedHandshake { ut_metadata: Some(3), metadata_size: None });
        let disabled = parse_extended_handshake(b"d1:md11:ut_metadatai0ee13:metadata_sizei9ee").unwrap();
        assert_eq!(disabled.ut_metadata, None);
        assert!(parse_extended_handshake(b"d1:md11:ut_metadatai999eee").unwrap().ut_metadata.is_none());
        assert!(parse_extended_handshake(b"not bencode").is_err());

        let ours = extended_handshake();
        assert_eq!(&ours[4..6], &[MSG_EXTENDED, EXT_HANDSHAKE]);
        assert_eq!(u32::from_be_bytes(ours[..4].try_into().unwrap()) as usize, ours.len() - 4);
        let parsed = parse_extended_handshake(&ours[6..]).unwrap();
        assert_eq!(parsed, ExtendedHandshake { ut_metadata: Some(UT_METADATA), metadata_size: None });
    }

    #[test]
    fn parses_metadata_messages() {
        let mut data = b"d8:msg_typei1e5:piecei0e10:total_sizei34256ee".to_vec();
        data.extend_from_slice(b"PIECE BYTES");
        assert_eq!(
            parse_metadata_message(&data).unwrap(),
            MetadataMessage::Data { piece: 0, total_size: Some(34256), data: b"PIECE BYTES" }
        );
        assert_eq!(parse_metadata_message(b"d8:msg_typei2e5:piecei7ee").unwrap(), MetadataMessage::Reject { piece: 7 });
        assert_eq!(
            parse_metadata_message(b"d8:msg_typei0e5:piecei1ee").unwrap(),
            MetadataMessage::Request { piece: 1 }
        );
        assert!(parse_metadata_message(b"d8:msg_typei9e5:piecei1ee").is_err());
        assert!(parse_metadata_message(b"d8:msg_typei1ee").is_err());
        assert!(parse_metadata_message(b"d8:msg_typei1e5:piecei-1ee").is_err());

        let request = metadata_request(2, 5);
        assert_eq!(&request[4..], b"\x14\x02d8:msg_typei0e5:piecei5ee");
    }

    #[test]
    fn assembles_and_verifies_pieces() {
        let (info, hash) = fixture();
        let mut pieces = Assembler::new(info.len() as u64, 10 << 20).unwrap();
        assert_eq!(pieces.count(), 3);
        // Out of order, with a repeat.
        for piece in [2u32, 0, 0, 1] {
            let start = piece as usize * PIECE;
            pieces.add(piece, &info[start..(start + PIECE).min(info.len())]).unwrap();
        }
        assert!(pieces.is_complete());
        assert_eq!(pieces.finish(&hash).unwrap(), info);

        let mut corrupt = Assembler::new(info.len() as u64, 10 << 20).unwrap();
        for piece in 0..3u32 {
            let start = piece as usize * PIECE;
            let mut data = info[start..(start + PIECE).min(info.len())].to_vec();
            data[10] ^= 0xff;
            corrupt.add(piece, &data).unwrap();
        }
        assert!(matches!(corrupt.finish(&hash), Err(PeerError::HashMismatch)));
    }

    #[test]
    fn rejects_bad_pieces_and_sizes() {
        assert!(matches!(Assembler::new(11 << 20, 10 << 20), Err(PeerError::TooLarge(_))));
        assert!(matches!(Assembler::new(u64::MAX, 10 << 20), Err(PeerError::TooLarge(_))));
        assert!(Assembler::new(0, 10 << 20).is_err());
        let mut pieces = Assembler::new(PIECE as u64 + 10, 10 << 20).unwrap();
        assert!(pieces.add(2, &[0; 10]).is_err(), "out of range");
        assert!(pieces.add(0, &[0; 10]).is_err(), "short middle piece");
        assert!(pieces.add(1, &[0; 11]).is_err(), "long last piece");
        assert!(pieces.add(1, &[0; 10]).is_ok());
        assert!(!pieces.is_complete());
        assert!(pieces.finish(&[0; 20]).is_err());
    }

    #[tokio::test]
    async fn fetches_metadata_from_a_peer() {
        let (info, _) = fixture();
        let (result, forbidden) = run(Behavior::Good).await;
        assert_eq!(result.unwrap(), info);
        assert!(forbidden.is_empty(), "client sent non-extension messages {forbidden:?}");
    }

    #[tokio::test]
    async fn refuses_misbehaving_peers() {
        let (result, forbidden) = run(Behavior::Corrupt).await;
        assert!(matches!(result, Err(PeerError::HashMismatch)), "{result:?}");
        assert!(forbidden.is_empty());
        let (result, _) = run(Behavior::Reject).await;
        assert!(matches!(result, Err(PeerError::Rejected(0))), "{result:?}");
        let (result, _) = run(Behavior::WrongTorrent).await;
        assert!(matches!(result, Err(PeerError::WrongTorrent)), "{result:?}");
        let (result, _) = run(Behavior::NoExtensions).await;
        assert!(matches!(result, Err(PeerError::NoExtensions)), "{result:?}");
        let (result, forbidden) = run(Behavior::Oversize(1 << 40)).await;
        assert!(matches!(result, Err(PeerError::TooLarge(_))), "{result:?}");
        assert!(forbidden.is_empty());
        let (result, _) = run(Behavior::Garbage).await;
        assert!(matches!(result, Err(PeerError::Protocol(_))), "{result:?}");
        let (result, _) = run(Behavior::Stall(Duration::from_millis(50))).await;
        assert!(matches!(result, Err(PeerError::Io(_))), "{result:?}");
    }

    #[tokio::test]
    async fn refuses_oversized_messages() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        server.write_all(&[0xff, 0xff, 0xff, 0xff, 20]).await.unwrap();
        let mut buf = Vec::new();
        let result = next_extended(&mut client, &mut buf).await;
        assert!(matches!(result, Err(PeerError::Protocol(_))), "{result:?}");
    }

    #[test]
    fn peer_ids_identify_the_client() {
        let (a, b) = (peer_id(), peer_id());
        assert_eq!(&a[..8], b"-OM0100-");
        assert_ne!(a, b);
    }
}

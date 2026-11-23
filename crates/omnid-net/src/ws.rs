//! A WebSocket client (RFC 6455), as much of one as reading Nostr relays
//! takes: the opening handshake, text messages (fragments joined), pings
//! answered and the closing handshake. Everything a server sends is capped:
//! the handshake's head, each message, and control frames.

use data_encoding::BASE64;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::BoxError;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The server's handshake response head, at most.
const MAX_HEAD: usize = 8 << 10;
/// Control frames carry 125 bytes at most (RFC 6455 §5.5).
const MAX_CONTROL: u64 = 125;

const CONTINUATION: u8 = 0x0;
const TEXT: u8 = 0x1;
const BINARY: u8 = 0x2;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

pub(crate) struct WebSocket<S> {
    stream: S,
    /// A message is refused, and the connection with it, past this many
    /// bytes.
    max_message: usize,
}

/// Opens a WebSocket on `stream` (already TLS where the URL is `wss://`):
/// `host` is the Host header, `path` the request target.
pub(crate) async fn connect<S>(
    mut stream: S,
    host: &str,
    path: &str,
    max_message: usize,
) -> Result<WebSocket<S>, BoxError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let key = BASE64.encode(&rand::random::<[u8; 16]>());
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    // A byte at a time, so nothing after the head is consumed: it is the
    // first frame.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err("oversized WebSocket handshake".into());
        }
        head.push(stream.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("101") {
        return Err(format!("not a WebSocket: {status}").into());
    }
    let accept = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
        .map(|(_, value)| value.trim().to_owned());
    if accept.as_deref() != Some(&accept_key(&key)) {
        return Err("the server's Sec-WebSocket-Accept does not match the key".into());
    }
    Ok(WebSocket { stream, max_message })
}

fn accept_key(key: &str) -> String {
    BASE64.encode(&Sha1::digest(format!("{key}{GUID}").as_bytes()))
}

impl<S: AsyncRead + AsyncWrite + Unpin> WebSocket<S> {
    pub(crate) async fn send_text(&mut self, text: &str) -> Result<(), BoxError> {
        self.send(TEXT, text.as_bytes()).await
    }

    /// The next text message; `None` once the server has closed the
    /// connection. Pings are answered on the way.
    pub(crate) async fn recv_text(&mut self) -> Result<Option<String>, BoxError> {
        let mut message = Vec::new();
        let mut in_message = false;
        loop {
            let (fin, opcode, payload) = self.frame().await?;
            match opcode {
                // Control frames may come between a message's fragments.
                PING => {
                    self.send(PONG, &payload).await?;
                    continue;
                }
                PONG => continue,
                CLOSE => {
                    // Echo the status code, as RFC 6455 asks; the server
                    // closes the TCP connection after.
                    let _ = self.send(CLOSE, payload.get(..2).unwrap_or_default()).await;
                    return Ok(None);
                }
                TEXT | BINARY if !in_message => {
                    if opcode == BINARY {
                        return Err("a binary message, where Nostr sends text".into());
                    }
                    message = payload;
                    in_message = true;
                }
                CONTINUATION if in_message => {
                    if message.len() + payload.len() > self.max_message {
                        return Err(format!("a message over {} bytes", self.max_message).into());
                    }
                    message.extend(payload);
                }
                _ => return Err(format!("unexpected WebSocket frame (opcode {opcode:#x})").into()),
            }
            if in_message && fin {
                return Ok(Some(String::from_utf8(message).map_err(|_| "a text message that is not UTF-8")?));
            }
        }
    }

    /// One frame: whether it is a message's last, its opcode and its payload.
    async fn frame(&mut self) -> Result<(bool, u8, Vec<u8>), BoxError> {
        let mut head = [0u8; 2];
        self.stream.read_exact(&mut head).await?;
        let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
        if head[0] & 0x70 != 0 {
            return Err("WebSocket extension bits set, though none was agreed".into());
        }
        if head[1] & 0x80 != 0 {
            return Err("a masked frame from the server".into());
        }
        let len = match head[1] & 0x7f {
            126 => u64::from(self.stream.read_u16().await?),
            127 => self.stream.read_u64().await?,
            len => u64::from(len),
        };
        let control = opcode & 0x8 != 0;
        if control && (len > MAX_CONTROL || !fin) {
            return Err("an oversized or fragmented control frame".into());
        }
        if len > self.max_message as u64 {
            return Err(format!("a frame over {} bytes", self.max_message).into());
        }
        let mut payload = vec![0u8; len as usize];
        self.stream.read_exact(&mut payload).await?;
        Ok((fin, opcode, payload))
    }

    /// A whole frame, masked as a client's must be.
    async fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), BoxError> {
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            len @ 0..126 => frame.push(0x80 | len as u8),
            len @ 126..=0xffff => {
                frame.push(0x80 | 126);
                frame.extend((len as u16).to_be_bytes());
            }
            len => {
                frame.push(0x80 | 127);
                frame.extend((len as u64).to_be_bytes());
            }
        }
        let mask: [u8; 4] = rand::random();
        frame.extend(mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }
}

/// For the fuzz targets in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::*;

    /// A server end that answers the handshake, then sends `frames` as given
    /// and closes; what the client sent after its handshake comes back.
    pub(crate) async fn stand_in(mut server: DuplexStream, frames: &[u8]) -> Vec<u8> {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            match server.read_u8().await {
                Ok(byte) => head.push(byte),
                Err(_) => return Vec::new(),
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let key = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).expect("a key");
        let reply = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(key)
        );
        // The client may have gone already, having refused a frame.
        if server.write_all(reply.as_bytes()).await.is_ok() && server.write_all(frames).await.is_ok() {
            let _ = server.shutdown().await;
        }
        let mut sent = Vec::new();
        let _ = server.read_to_end(&mut sent).await;
        sent
    }

    /// Room in each direction for everything either end sends, so neither
    /// waits on the other: a 2-byte ping is answered with a 6-byte pong.
    pub(crate) fn pipe(input: &[u8]) -> (DuplexStream, DuplexStream) {
        tokio::io::duplex(4 * input.len() + (64 << 10))
    }

    /// What a server sends after the handshake, read message by message.
    /// `input`'s first byte picks the message cap; the rest is the frames.
    /// A message comes out whole, UTF-8 and within the cap, and the client
    /// answers with masked pongs and closes only.
    pub fn frames(input: &[u8]) {
        let Some((&pick, frames)) = input.split_first() else { return };
        let max_message = [0, 125, 1 << 10, 64 << 10][usize::from(pick & 3)];
        let runtime = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
        let (client, server) = pipe(input);
        let read = async {
            let mut ws = connect(client, "relay.example", "/", max_message).await.expect("a handshake");
            // Each message takes at least one 2-byte frame.
            for _ in 0..=frames.len() / 2 {
                match ws.recv_text().await {
                    Ok(Some(message)) => assert!(message.len() <= max_message, "{} bytes", message.len()),
                    Ok(None) | Err(_) => break,
                }
            }
        };
        let (sent, ()) = runtime.block_on(async { tokio::join!(stand_in(server, frames), read) });
        let mut rest = &sent[..];
        while let [first, second, tail @ ..] = rest {
            assert!(first & 0x80 != 0 && matches!(first & 0x0f, PONG | CLOSE), "sent opcode {first:#x}");
            assert!(second & 0x80 != 0, "an unmasked frame from the client");
            let len = usize::from(second & 0x7f);
            assert!(len as u64 <= MAX_CONTROL);
            rest = tail.get(4 + len..).expect("a whole frame");
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

    use super::*;

    #[test]
    fn accept_key_is_rfc_6455s_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// A server end that answers the handshake, then sends `frames` as given.
    async fn server(frames: Vec<u8>) -> WebSocket<DuplexStream> {
        let (client, mut server) = duplex(1 << 20);
        tokio::spawn(async move {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(server.read_u8().await.unwrap());
            }
            let head = String::from_utf8(head).unwrap();
            let key = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap();
            let reply = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(key)
            );
            server.write_all(reply.as_bytes()).await.unwrap();
            server.write_all(&frames).await.unwrap();
            // Keep the pipe open for what the client writes back.
            let mut sink = Vec::new();
            let _ = server.read_to_end(&mut sink).await;
        });
        connect(client, "relay.example", "/", 1024).await.unwrap()
    }

    fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 } else { 0 } | opcode];
        if payload.len() < 126 {
            f.push(payload.len() as u8);
        } else {
            f.push(126);
            f.extend((payload.len() as u16).to_be_bytes());
        }
        f.extend(payload);
        f
    }

    #[tokio::test]
    async fn joins_fragments_answers_pings_and_ends_at_close() {
        let frames = [
            frame(false, TEXT, b"[\"EVENT\","),
            frame(true, PING, b"hi"),
            frame(true, CONTINUATION, b"1]"),
            frame(true, TEXT, &[b'x'; 200]),
            frame(true, CLOSE, &[0x03, 0xe8]),
        ]
        .concat();
        let mut ws = server(frames).await;
        assert_eq!(ws.recv_text().await.unwrap().as_deref(), Some("[\"EVENT\",1]"));
        assert_eq!(ws.recv_text().await.unwrap().map(|m| m.len()), Some(200), "a 16-bit length");
        assert_eq!(ws.recv_text().await.unwrap(), None);
    }

    #[tokio::test]
    async fn refuses_what_a_server_must_not_send() {
        let oversized = [&[0x81, 126][..], &2000u16.to_be_bytes()].concat();
        for (frames, why) in [
            (oversized, "a frame over the cap"),
            ([frame(false, TEXT, b"a"), frame(true, CONTINUATION, &[b'b'; 1100])].concat(), "fragments over the cap"),
            (vec![0x81, 0x81, 1, 2, 3, 4, b'x'], "masked"),
            (frame(false, PING, b""), "a fragmented ping"),
            (frame(true, BINARY, b"\x00"), "binary"),
            (frame(true, CONTINUATION, b"x"), "a continuation with nothing to continue"),
            (frame(true, TEXT, &[0xff, 0xfe]), "not UTF-8"),
            (vec![0xc1, 0x00], "an extension bit"),
        ] {
            let mut ws = server(frames).await;
            assert!(ws.recv_text().await.is_err(), "{why}");
        }
    }

    #[tokio::test]
    async fn checks_the_servers_accept_key() {
        let (client, mut server) = duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let _ = server.read(&mut buf).await;
            let reply = "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: wrong=\r\n\r\n";
            server.write_all(reply.as_bytes()).await.unwrap();
        });
        assert!(connect(client, "relay.example", "/", 1024).await.is_err());
        let (client, mut server) = duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let _ = server.read(&mut buf).await;
            server.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        });
        assert!(connect(client, "relay.example", "/", 1024).await.is_err(), "not 101");
    }
}

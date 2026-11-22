//! A stand-in Nostr relay for tests, on 127.0.0.1 over plain `ws://`: it
//! answers a REQ with the stored events its filter matches, then EOSE, and
//! signs events with test keys. Shared by omnid-net's and omnid-crawl's
//! tests (`#[path]`).

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use data_encoding::BASE64;
use k256::schnorr::SigningKey;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_slice(&[seed; 32]).unwrap()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn pubkey(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes().into()
}

/// An event as a relay sends it, signed by `key`.
pub fn signed(key: &SigningKey, created_at: u64, kind: u32, tags: Value, content: &str) -> Value {
    let pubkey = hex(&pubkey(key));
    let serialized = json!([0, pubkey, created_at, kind, tags, content]).to_string();
    let id: [u8; 32] = Sha256::digest(serialized.as_bytes()).into();
    let sig = key.sign_raw(&id, &[0; 32]).unwrap();
    json!({"id": hex(&id), "pubkey": pubkey, "created_at": created_at, "kind": kind, "tags": tags,
           "content": content, "sig": hex(&sig.to_bytes())})
}

/// The relay's address and the REQ filters it was sent.
pub struct Relay {
    pub url: String,
    pub asked: Arc<Mutex<Vec<Value>>>,
}

pub async fn relay(events: Vec<Value>) -> Relay {
    serve_on(events, true).await
}

/// A relay that sends every event it has, whatever the filter.
pub async fn careless_relay(events: Vec<Value>) -> Relay {
    serve_on(events, false).await
}

async fn serve_on(events: Vec<Value>, filters: bool) -> Relay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (events, log) = (Arc::new(events), asked.clone());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (events, log) = (events.clone(), log.clone());
            tokio::spawn(async move {
                let _ = serve(stream, &events, &log, filters).await;
            });
        }
    });
    Relay { url, asked }
}

async fn serve(
    mut stream: TcpStream,
    events: &[Value],
    asked: &Mutex<Vec<Value>>,
    filters: bool,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let key = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap_or_default();
    let accept = BASE64.encode(&Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
    let reply = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(reply.as_bytes()).await?;
    loop {
        let message = read_frame(&mut stream).await?;
        let Ok(Value::Array(message)) = serde_json::from_str::<Value>(&message) else { continue };
        if message.first().and_then(Value::as_str) != Some("REQ") {
            continue;
        }
        let sub = message[1].clone();
        let filter = message[2].clone();
        asked.lock().unwrap().push(filter.clone());
        for event in events.iter().filter(|e| !filters || matches(&filter, e)) {
            write_frame(&mut stream, &json!(["EVENT", sub, event]).to_string()).await?;
        }
        write_frame(&mut stream, &json!(["EOSE", sub]).to_string()).await?;
    }
}

fn matches(filter: &Value, event: &Value) -> bool {
    let has = |field: &str, value: &Value| {
        filter.get(field).and_then(Value::as_array).is_none_or(|list| list.contains(value))
    };
    let tag = |name: &str| {
        filter.get(format!("#{name}")).and_then(Value::as_array).is_none_or(|wanted| {
            event["tags"].as_array().unwrap().iter().any(|t| t[0] == name && wanted.contains(&t[1]))
        })
    };
    has("ids", &event["id"]) && has("authors", &event["pubkey"]) && has("kinds", &event["kind"]) && tag("e") && tag("d")
}

async fn read_frame(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    let len = match head[1] & 0x7f {
        126 => usize::from(stream.read_u16().await?),
        127 => stream.read_u64().await? as usize,
        len => usize::from(len),
    };
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask).await?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    if head[0] & 0x0f == 0x8 {
        return Err(std::io::ErrorKind::ConnectionAborted.into());
    }
    Ok(String::from_utf8_lossy(&payload).into_owned())
}

async fn write_frame(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let mut frame = vec![0x81];
    match text.len() {
        len @ 0..126 => frame.push(len as u8),
        len @ 126..=0xffff => {
            frame.push(126);
            frame.extend((len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend((len as u64).to_be_bytes());
        }
    }
    frame.extend(text.as_bytes());
    stream.write_all(&frame).await
}

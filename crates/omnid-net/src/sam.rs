//! Separate I2P identities through SAM, the interface an I2P router offers
//! programs for making destinations of their own. Through an i2pd HTTP proxy
//! every request comes from the proxy's one destination, so eepsites can link
//! the visits of every site and app behind it to each other. Here each
//! isolation key, the site the browser is on or `app:<user>` for the proxy for
//! other apps, gets a session of its own: a transient destination, never
//! published, on 3-hop tunnels whatever the router's own defaults are (the
//! i2pd app ships 1 hop for its proxy).
//!
//! A session's tunnels take 5 to 60 s to build, so while a bridge answers
//! [`SPARES`] are kept built for the next new sites, and made together so the
//! first done serves a site that is waiting. A site's session idle for
//! [`IDLE`] is closed, at most [`MAX_KEPT`] are kept (the least recently used
//! goes first), and [`Sam::reset`] closes them for a new identity; the spares,
//! never used, stay. Without a bridge that answers, [`Sam::stream`] says so
//! and the caller uses the HTTP proxies, as before.
//!
//! The protocol is SAM v3 (<https://i2p.net/en/docs/api/samv3/>). The bridge
//! is the owner's router, but what it sends is bounded all the same: a line is
//! at most [`MAX_LINE`] bytes, and at most [`MAX_OPEN`] sessions are open.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use omnid_core::Network;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{Notify, OnceCell};
use tokio_util::sync::{CancellationToken, DropGuard};
use url::Url;

use crate::routers::{self, Routers};
use crate::tor::ONE_OFF;
use crate::{BoxError, FetchError};

/// Where a router's SAM bridge listens by default.
pub const DEFAULT_PORT: u16 = 7656;

const HELLO: &[u8] = b"HELLO VERSION MIN=3.1 MAX=3.3\n";
/// Every session: a new destination (signature type 7, Ed25519; encryption
/// type 4, ECIES-X25519, with ElGamal for old routers), 3 hops and 2 tunnels
/// each way, and a leaseset that is not published: a client's travels with
/// its messages.
const SESSION: &str = "DESTINATION=TRANSIENT SIGNATURE_TYPE=7 inbound.length=3 outbound.length=3 \
     inbound.quantity=2 outbound.quantity=2 i2cp.dontPublishLeaseSet=true i2cp.leaseSetEncType=4,0";
/// The longest line read from a bridge; a session's reply, with its keys, is
/// about 1 KiB.
pub(crate) const MAX_LINE: usize = 8 << 10;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A new session's tunnels take 5 to 60 s to build, more on a router that has
/// just started; one that is slower than the browser waits is still a spare.
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a new site waits for a spare, longer than making one can take.
const WAIT: Duration = Duration::from_secs(150);
/// A session's control connection says something this often, so a relay that
/// closes quiet connections (the one in the README's setup does after ten
/// minutes) keeps it open.
const PING_EVERY: Duration = Duration::from_secs(120);
/// With text after it: i2pd 3.0 closes the session on a bare `PING`.
const PING: &[u8] = b"PING keepalive\n";
/// A session no stream has used for this long, with none open, is closed.
const IDLE: Duration = Duration::from_secs(600);
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// Sessions kept for their keys; past this, the least recently used goes.
pub(crate) const MAX_KEPT: usize = 12;
/// Sessions made ahead for the next new sites.
pub(crate) const SPARES: usize = 2;
/// Sessions open at once: kept, spare, used once, or no longer kept but still
/// carrying a stream.
pub(crate) const MAX_OPEN: usize = 32;

/// The SAM bridges of the routers whose HTTP proxies are `proxies`: each
/// host's, on [`DEFAULT_PORT`], once.
pub fn beside(proxies: &[Url]) -> Vec<Url> {
    let mut bridges: Vec<Url> = Vec::new();
    for proxy in proxies {
        let Some(bridge) = proxy.host_str().and_then(|host| format!("sam://{host}:{DEFAULT_PORT}").parse().ok()) else {
            continue;
        };
        if !bridges.contains(&bridge) {
            bridges.push(bridge);
        }
    }
    bridges
}

pub(crate) struct Sam(Arc<Inner>);

struct Inner {
    bridges: Routers<()>,
    state: Mutex<State>,
    /// Woken whenever making a spare finishes, well or not.
    made: Notify,
    open: Arc<AtomicUsize>,
}

#[derive(Default)]
struct State {
    kept: HashMap<String, Kept>,
    spares: Vec<Arc<Session>>,
    /// Spares being made, and how many attempts have finished.
    building: usize,
    finished: u64,
    /// Why the last attempt to finish made no session; `None` when it made one.
    failure: Option<Failure>,
    sweeping: bool,
}

struct Kept {
    /// Made once, by the first stream for the key; the others wait for it.
    session: Arc<OnceCell<Arc<Session>>>,
    used: Instant,
}

/// A destination the router keeps while the session's control connection is
/// open; dropping the last handle closes it.
struct Session {
    id: String,
    bridge: Url,
    /// Set when the bridge closed the control connection or forgot the
    /// session: its streams fail from then on.
    closed: Arc<AtomicBool>,
    _control: DropGuard,
    _open: Opened,
}

impl Session {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

/// One of the [`MAX_OPEN`] sessions, counted until dropped.
struct Opened(Arc<AtomicUsize>);

impl Opened {
    fn new(open: &Arc<AtomicUsize>) -> Result<Self, String> {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_OPEN {
            open.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("{MAX_OPEN} I2P identities are open already"));
        }
        Ok(Self(open.clone()))
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why no session was had, as told to every stream that waited for one.
#[derive(Clone)]
enum Failure {
    /// No bridge answered: the caller goes on without SAM.
    NoBridge,
    Error(String),
}

impl From<FetchError> for Failure {
    fn from(error: FetchError) -> Self {
        Failure::Error(match error {
            FetchError::Transport { error, .. } => error.to_string(),
            error => error.to_string(),
        })
    }
}

impl Sam {
    /// `bridges` are `sam://host:port` URLs, best first; a missing port is
    /// [`DEFAULT_PORT`].
    pub(crate) fn new(bridges: &[Url]) -> Result<Self, BoxError> {
        let mut urls = Vec::with_capacity(bridges.len());
        for bridge in bridges {
            if bridge.scheme() != "sam" || bridge.host_str().is_none_or(str::is_empty) {
                return Err(format!("a SAM bridge is sam://host:port, not {bridge}").into());
            }
            let mut url = bridge.clone();
            if url.port().is_none() {
                url.set_port(Some(DEFAULT_PORT)).map_err(|()| format!("bad SAM bridge {bridge}"))?;
            }
            urls.push(url);
        }
        let bridges = Routers::new(Network::I2p, &urls, |_| Ok(()))?;
        Ok(Self(Arc::new(Inner { bridges, state: Mutex::default(), made: Notify::new(), open: Arc::default() })))
    }

    /// A stream to `host:port`, on `key`'s session or, with `None`, on one of
    /// its own that is not kept. `Ok(None)` when no bridge answers.
    pub(crate) async fn stream(
        &self,
        host: &str,
        port: u16,
        key: Option<&str>,
    ) -> Result<Option<SamStream>, FetchError> {
        if !valid_host(host) {
            return Err(transport(format!("{host:?} is not an I2P host name")));
        }
        // Tried a second time when that may do better: a session the router
        // forgot (it restarted, say) is made again, and a site whose leaseset
        // a new destination did not find at first is looked up again.
        let mut retried = false;
        loop {
            let session = match self.0.session(key).await {
                Ok(session) => session,
                Err(Failure::NoBridge) => return Ok(None),
                Err(Failure::Error(error)) => return Err(transport(error)),
            };
            match self.0.connect(&session, host, port).await {
                Ok(stream) => return Ok(Some(stream)),
                Err((_, true)) if !retried => retried = true,
                Err((error, _)) => return Err(error),
            }
        }
    }

    /// Closes every site's session not carrying a stream now, and the rest as
    /// their streams end: sites get new identities from their next stream on.
    /// The spares stay: no site has used them.
    pub(crate) fn reset(&self) {
        self.0.lock().kept.clear();
    }

    /// Each bridge's state, for the I2P router's status, and whether sites
    /// get identities of their own.
    pub(crate) async fn describe(&self) -> String {
        let all = self.0.bridges.all();
        if all.is_empty() {
            return "one I2P identity for every site (no SAM bridge set)".into();
        }
        let mut probes = tokio::task::JoinSet::new();
        for (i, bridge) in all.iter().enumerate() {
            let url = bridge.url.clone();
            probes.spawn(async move { (i, hello(&url).await.is_ok()) });
        }
        let mut up = vec![false; all.len()];
        while let Some(Ok((i, ok))) = probes.join_next().await {
            up[i] = ok;
        }
        let mut parts = Vec::new();
        for (bridge, ok) in all.iter().zip(&up) {
            if *ok {
                bridge.succeeded();
                parts.push(format!("SAM at {}", bridge.url));
            } else {
                bridge.failed();
                let why =
                    if routers::askable(Network::I2p, &bridge.url).is_err() { " (not on its tailnet now)" } else { "" };
                parts.push(format!("no SAM at {}{why}", bridge.url));
            }
        }
        if up.contains(&true) {
            parts.push("an I2P identity per site".into());
            // The daemon asks for the status every half minute: the spares
            // are made, and made again, while a bridge answers.
            self.0.replenish();
        } else {
            parts.push("one I2P identity for every site".into());
        }
        parts.join("; ")
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("SAM lock poisoned")
    }

    /// `key`'s session, made when it has none (or one that closed); a key
    /// that starts with [`ONE_OFF`], or none, gets one of its own.
    async fn session(self: &Arc<Self>, key: Option<&str>) -> Result<Arc<Session>, Failure> {
        let Some(key) = key.filter(|key| !key.starts_with(ONE_OFF)) else {
            return self.fresh().await;
        };
        let cell = {
            let mut state = self.lock();
            let now = Instant::now();
            if state.kept.get(key).and_then(|kept| kept.session.get()).is_some_and(|s| s.is_closed()) {
                state.kept.remove(key);
            }
            if !state.kept.contains_key(key) && state.kept.len() >= MAX_KEPT {
                let oldest = state.kept.iter().min_by_key(|(_, kept)| kept.used).map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    state.kept.remove(&oldest);
                }
            }
            let kept = state.kept.entry(key.to_owned()).or_insert_with(|| Kept { session: Arc::default(), used: now });
            kept.used = now;
            kept.session.clone()
        };
        self.sweep_later();
        cell.get_or_try_init(|| self.fresh()).await.cloned()
    }

    /// A spare, waiting for one when none is ready: [`SPARES`] are made at
    /// once, and the first done is taken.
    async fn fresh(self: &Arc<Self>) -> Result<Arc<Session>, Failure> {
        let mut since = {
            let mut state = self.lock();
            state.spares.retain(|spare| !spare.is_closed());
            if state.spares.is_empty() && state.building == 0 && self.bridges.awake().is_empty() {
                return Err(Failure::NoBridge);
            }
            state.finished
        };
        self.replenish();
        let wait = async {
            loop {
                let made = self.made.notified();
                tokio::pin!(made);
                made.as_mut().enable();
                let idle = {
                    let mut state = self.lock();
                    state.spares.retain(|spare| !spare.is_closed());
                    if let Some(spare) = state.spares.pop() {
                        return Ok(spare);
                    }
                    if state.building == 0
                        && state.finished > since
                        && let Some(failure) = &state.failure
                    {
                        return Err(failure.clone());
                    }
                    // Nothing is being made, or what was made went to other
                    // sites first: more, then.
                    if state.building == 0 {
                        since = state.finished;
                    }
                    state.building == 0
                };
                if idle {
                    self.replenish();
                }
                made.await;
            }
        };
        let taken = tokio::time::timeout(WAIT, wait)
            .await
            .unwrap_or_else(|_| Err(Failure::Error(format!("no I2P session was made in {WAIT:?}"))));
        self.replenish();
        taken
    }

    /// Starts making spares until [`SPARES`] are ready or being made.
    fn replenish(self: &Arc<Self>) {
        let wanted = {
            let mut state = self.lock();
            state.spares.retain(|spare| !spare.is_closed());
            let wanted = SPARES.saturating_sub(state.spares.len() + state.building);
            state.building += wanted;
            wanted
        };
        for _ in 0..wanted {
            let inner = self.clone();
            tokio::spawn(async move {
                let made = inner.create().await;
                {
                    let mut state = inner.lock();
                    state.building -= 1;
                    state.finished += 1;
                    match made {
                        Ok(session) => {
                            state.failure = None;
                            state.spares.push(session);
                        }
                        Err(failure) => state.failure = Some(failure),
                    }
                }
                inner.made.notify_waiters();
            });
        }
    }

    /// A new session on the best bridge that answers.
    async fn create(&self) -> Result<Arc<Session>, Failure> {
        let opened = Opened::new(&self.open).map_err(Failure::Error)?;
        for bridge in self.bridges.awake() {
            match hello(&bridge.url).await {
                Ok(control) => {
                    bridge.succeeded();
                    return Ok(start(bridge.url.clone(), control, opened).await?);
                }
                Err(_) => bridge.failed(),
            }
        }
        Err(Failure::NoBridge)
    }

    /// A stream to `host:port` from `session`'s destination, or why not and
    /// whether a second try may do better. When the bridge is gone or has
    /// forgotten the session, the session is marked closed.
    async fn connect(&self, session: &Arc<Session>, host: &str, port: u16) -> Result<SamStream, (FetchError, bool)> {
        let mut socket = match hello(&session.bridge).await {
            Ok(socket) => socket,
            Err(error) => {
                session.closed.store(true, Ordering::Relaxed);
                if let Some(bridge) = self.bridges.all().iter().find(|b| b.url == session.bridge) {
                    bridge.failed();
                }
                return Err((error, true));
            }
        };
        let command = format!("STREAM CONNECT ID={} DESTINATION={host} TO_PORT={port} SILENT=false\n", session.id);
        let once = |error: FetchError| (error, false);
        socket.get_mut().write_all(command.as_bytes()).await.map_err(|e| once(transport(e)))?;
        let line = read_line(&mut socket).await.map_err(|e| once(transport(e)))?;
        let reply = Reply::parse(&line);
        if !reply.is("STREAM", "STATUS") {
            return Err(once(transport(format!("the SAM bridge answered {:?}", reply.topic()))));
        }
        match reply.get("RESULT") {
            Some("OK") => Ok(SamStream { socket, _session: session.clone() }),
            Some("INVALID_ID") => {
                session.closed.store(true, Ordering::Relaxed);
                Err((transport("the router no longer has this site's I2P session"), true))
            }
            Some("INVALID_KEY") => {
                Err(once(transport(format!("the router's address book has no {host}; try its .b32.i2p address"))))
            }
            Some("CANT_REACH_PEER" | "PEER_NOT_FOUND" | "TIMEOUT") => {
                Err((transport(format!("{host} did not answer over I2P{}", reply.message())), true))
            }
            result => Err(once(transport(format!(
                "the SAM bridge refused the stream: {}{}",
                result.unwrap_or("?"),
                reply.message()
            )))),
        }
    }

    /// Starts the sweeper unless it runs: it closes idle sessions and ends
    /// once no site has one.
    fn sweep_later(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.sweeping {
                return;
            }
            state.sweeping = true;
        }
        let inner = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_EVERY).await;
                let Some(inner) = inner.upgrade() else { return };
                let mut state = inner.lock();
                if !state.sweep(Instant::now()) {
                    state.sweeping = false;
                    return;
                }
            }
        });
    }
}

impl State {
    /// Closes the sites' sessions that have been idle for [`IDLE`] and that no
    /// stream holds, and those the bridge closed. Whether any is left to sweep
    /// later.
    fn sweep(&mut self, now: Instant) -> bool {
        self.kept.retain(|_, kept| {
            let idle = now.duration_since(kept.used) >= IDLE;
            // The map's handle is the only one: no stream is open on it, and
            // no stream is waiting for it to be made.
            let unused = Arc::strong_count(&kept.session) == 1
                && kept.session.get().is_none_or(|session| Arc::strong_count(session) == 1);
            let closed = kept.session.get().is_some_and(|session| session.is_closed());
            !(closed || (idle && unused))
        });
        !self.kept.is_empty()
    }
}

/// Makes the session on `control`, a connection past HELLO, and keeps that
/// connection open for it.
async fn start(bridge: Url, mut control: BufReader<TcpStream>, opened: Opened) -> Result<Arc<Session>, FetchError> {
    let id = format!("omnid-{:016x}", rand::random::<u64>());
    let command = format!("SESSION CREATE STYLE=STREAM ID={id} {SESSION}\n");
    let reply = tokio::time::timeout(CREATE_TIMEOUT, async {
        control.get_mut().write_all(command.as_bytes()).await?;
        read_line(&mut control).await
    })
    .await
    .map_err(|_| FetchError::Timeout(CREATE_TIMEOUT))?
    .map_err(transport)?;
    // The reply carries the destination's private keys: only its result is
    // looked at, and it is dropped here.
    let reply = Reply::parse(&reply);
    if !reply.is("SESSION", "STATUS") || reply.get("RESULT") != Some("OK") {
        let result = reply.get("RESULT").unwrap_or("no result");
        return Err(transport(format!(
            "the SAM bridge at {bridge} would not make a session: {result}{}",
            reply.message()
        )));
    }
    let stop = CancellationToken::new();
    let closed = Arc::new(AtomicBool::new(false));
    tokio::spawn(keep(control, stop.clone(), closed.clone(), PING_EVERY));
    Ok(Arc::new(Session { id, bridge, closed, _control: stop.drop_guard(), _open: opened }))
}

/// Holds a session's control connection open until `stop`, answering the
/// bridge's PING and sending its own. When the bridge closes it, the
/// destination is gone and `closed` says so.
async fn keep<S: AsyncRead + AsyncWrite + Unpin>(
    mut control: BufReader<S>,
    stop: CancellationToken,
    closed: Arc<AtomicBool>,
    every: Duration,
) {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    let mut line = Vec::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = ping.tick() => {
                if control.get_mut().write_all(PING).await.is_err() {
                    break;
                }
            }
            read = read_into(&mut control, &mut line) => {
                if read.is_err() {
                    break;
                }
                let text = String::from_utf8_lossy(&line);
                if let Some(rest) = text.trim_end().strip_prefix("PING") {
                    let pong = format!("PONG{rest}\n");
                    if control.get_mut().write_all(pong.as_bytes()).await.is_err() {
                        break;
                    }
                }
                line.clear();
            }
        }
    }
    closed.store(true, Ordering::Relaxed);
}

/// A connection to the SAM bridge at `bridge`, past HELLO.
async fn hello(bridge: &Url) -> Result<BufReader<TcpStream>, FetchError> {
    let mut socket = BufReader::new(routers::connect(Network::I2p, bridge).await?);
    let reply = tokio::time::timeout(HELLO_TIMEOUT, async {
        socket.get_mut().write_all(HELLO).await?;
        read_line(&mut socket).await
    })
    .await;
    if let Ok(Ok(line)) = reply {
        let reply = Reply::parse(&line);
        if reply.is("HELLO", "REPLY") && reply.get("RESULT") == Some("OK") {
            return Ok(socket);
        }
    }
    Err(FetchError::Unavailable { network: Network::I2p, reason: format!("no SAM bridge answers at {bridge}") })
}

/// Reads up to a newline into `line`. What was read stays in `line` when the
/// future is dropped first, so it can go on in a `select!`. Fails past
/// [`MAX_LINE`] bytes and at the end of the stream.
async fn read_into<R: AsyncBufRead + Unpin>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (taken, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        line.extend_from_slice(&available[..taken]);
        reader.consume(taken);
        if line.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a line from the SAM bridge over 8 KiB"));
        }
        if done {
            return Ok(());
        }
    }
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<String> {
    let mut line = Vec::new();
    read_into(reader, &mut line).await?;
    Ok(String::from_utf8_lossy(&line).trim_end().to_owned())
}

/// Whether `host` can go into a SAM command as it is: an I2P name of
/// lower-case letters, digits, dots and hyphens.
fn valid_host(host: &str) -> bool {
    host.len() <= 255
        && host.len() > ".i2p".len()
        && host.ends_with(".i2p")
        && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

fn transport(error: impl Into<BoxError>) -> FetchError {
    FetchError::Transport { network: Network::I2p, error: error.into() }
}

/// A line from the bridge: `TOPIC SUBTOPIC KEY=VALUE …`, a value in quotes
/// when it holds spaces.
struct Reply<'a> {
    words: Vec<&'a str>,
}

impl<'a> Reply<'a> {
    fn parse(line: &'a str) -> Self {
        let mut words = Vec::new();
        let mut start = None;
        let mut quoted = false;
        for (i, c) in line.char_indices() {
            if c == ' ' && !quoted {
                if let Some(s) = start.take() {
                    words.push(&line[s..i]);
                }
                continue;
            }
            if c == '"' {
                quoted = !quoted;
            }
            start.get_or_insert(i);
        }
        if let Some(s) = start {
            words.push(&line[s..]);
        }
        Self { words }
    }

    fn is(&self, topic: &str, subtopic: &str) -> bool {
        self.words.len() >= 2 && self.words[0] == topic && self.words[1] == subtopic
    }

    fn topic(&self) -> String {
        self.words.iter().take(2).copied().collect::<Vec<_>>().join(" ")
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        self.words
            .iter()
            .skip(2)
            .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim_matches('"'))
    }

    /// `: MESSAGE`, when the bridge gave one.
    fn message(&self) -> String {
        self.get("MESSAGE").map(|m| format!(": {}", m.chars().take(200).collect::<String>())).unwrap_or_default()
    }
}

/// A stream through SAM. It holds its session open while it lasts.
pub(crate) struct SamStream {
    socket: BufReader<TcpStream>,
    _session: Arc<Session>,
}

impl AsyncRead for SamStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for SamStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().socket).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
    }
}

/// For the fuzz target in `fuzz/`.
#[cfg(fuzzing)]
pub(crate) mod fuzz {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// What a SAM bridge sends, read line by line as the client reads it, and
    /// each line parsed as a reply.
    pub fn replies(input: &[u8]) {
        let mut reader = tokio::io::BufReader::new(input);
        loop {
            // The bytes are all there, so a read never waits.
            let Poll::Ready(read) = pin!(read_line(&mut reader)).poll(&mut Context::from_waker(Waker::noop())) else {
                unreachable!("a read from memory waited");
            };
            let Ok(line) = read else { break };
            // At most MAX_LINE bytes were read, each at most a U+FFFD.
            assert!(line.len() <= 3 * MAX_LINE);
            let reply = Reply::parse(&line);
            let _ = (reply.is("STREAM", "STATUS"), reply.get("RESULT"), reply.topic());
            assert!(reply.message().chars().count() <= ": ".len() + 200);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_replies_with_quoted_values() {
        let reply = Reply::parse(r#"SESSION STATUS RESULT=I2P_ERROR MESSAGE="no tunnels yet, RESULT=OK""#);
        assert!(reply.is("SESSION", "STATUS"));
        assert_eq!(reply.get("RESULT"), Some("I2P_ERROR"), "a quoted value does not end a word at its spaces");
        assert_eq!(reply.get("MESSAGE"), Some("no tunnels yet, RESULT=OK"));
        assert_eq!(reply.message(), ": no tunnels yet, RESULT=OK");
        let ok = Reply::parse("STREAM STATUS  RESULT=OK");
        assert!(ok.is("STREAM", "STATUS") && ok.get("RESULT") == Some("OK") && ok.get("RES").is_none());
        assert!(!Reply::parse("STREAM").is("STREAM", "STATUS") && Reply::parse("").get("RESULT").is_none());
    }

    #[tokio::test]
    async fn a_line_is_bounded_and_kept_across_calls() {
        let mut long = vec![b'A'; MAX_LINE + 1];
        long.push(b'\n');
        let mut reader = BufReader::new(&long[..]);
        let error = read_line(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut reader = BufReader::with_capacity(4, &b"PING abcdef\nPONG\n"[..]);
        let mut line = Vec::new();
        read_into(&mut reader, &mut line).await.unwrap();
        assert_eq!(line, b"PING abcdef\n", "read in pieces smaller than the line");
        assert_eq!(read_line(&mut reader).await.unwrap(), "PONG");
        assert_eq!(read_line(&mut reader).await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn keeps_the_control_connection_open_as_i2pd_wants() {
        let (ours, bridge) = tokio::io::duplex(1024);
        let (stop, closed) = (CancellationToken::new(), Arc::new(AtomicBool::new(false)));
        let kept = tokio::spawn(keep(BufReader::new(ours), stop.clone(), closed.clone(), Duration::from_millis(20)));
        let mut bridge = BufReader::new(bridge);
        assert_eq!(read_line(&mut bridge).await.unwrap(), "PING keepalive", "never a bare PING, which i2pd ends");
        bridge.get_mut().write_all(b"PING 1795\n").await.unwrap();
        let mut pong = read_line(&mut bridge).await.unwrap();
        while pong.starts_with("PING") {
            pong = read_line(&mut bridge).await.unwrap();
        }
        assert_eq!(pong, "PONG 1795");
        assert!(!closed.load(Ordering::Relaxed));
        drop(bridge);
        kept.await.unwrap();
        assert!(closed.load(Ordering::Relaxed), "the bridge closed it: the session is gone");
        stop.cancel();
    }

    #[test]
    fn only_i2p_names_go_into_a_command() {
        assert!(valid_host("reg.i2p") && valid_host("mqtlargpv4247iylywxw6ibi6qpz6my5duqm33c4lcdhjg5yfh7q.b32.i2p"));
        for bad in ["reg.i2p\nSESSION CREATE", "reg.i2p ID=x", "REG.I2P", "example.com", ".i2p", "a=b.i2p"] {
            assert!(!valid_host(bad), "{bad:?}");
        }
        assert!(!valid_host(&format!("{}.i2p", "a".repeat(252))));
    }

    #[test]
    fn bridges_are_sam_urls_with_a_port() {
        let sam = Sam::new(&["sam://127.0.0.1".parse().unwrap(), "sam://100.74.26.84:7657".parse().unwrap()]).unwrap();
        let urls: Vec<String> = sam.0.bridges.urls().map(Url::to_string).collect();
        assert_eq!(urls, ["sam://127.0.0.1:7656", "sam://100.74.26.84:7657"]);
        assert!(Sam::new(&["http://127.0.0.1:7656".parse().unwrap()]).is_err());
    }

    #[test]
    fn a_routers_bridge_is_beside_its_proxy() {
        let proxies: Vec<Url> =
            ["http://100.74.26.84:4444/", "http://127.0.0.1:4444/", "http://127.0.0.1:4445/", "http://[::1]:4444/"]
                .iter()
                .map(|p| p.parse().unwrap())
                .collect();
        let bridges: Vec<String> = beside(&proxies).iter().map(Url::to_string).collect();
        assert_eq!(bridges, ["sam://100.74.26.84:7656", "sam://127.0.0.1:7656", "sam://[::1]:7656"]);
        assert!(Sam::new(&beside(&proxies)).is_ok());
    }

    #[test]
    fn idle_sessions_are_swept() {
        let mut state = State::default();
        let now = Instant::now();
        let cell = Arc::new(OnceCell::new());
        state.kept.insert("waiting".into(), Kept { session: cell.clone(), used: now - IDLE });
        state.kept.insert("idle".into(), Kept { session: Arc::default(), used: now - IDLE });
        state.kept.insert("recent".into(), Kept { session: Arc::default(), used: now });
        assert!(state.sweep(now));
        let mut left: Vec<_> = state.kept.keys().cloned().collect();
        left.sort();
        assert_eq!(left, ["recent", "waiting"], "a session still being made for a stream stays");
        drop(cell);
        state.kept.get_mut("recent").unwrap().used = now - IDLE;
        assert!(!state.sweep(now), "nothing left");
    }
}

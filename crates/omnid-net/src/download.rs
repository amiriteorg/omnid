//! Downloads a user asked for: one file, in byte ranges fetched over several
//! connections at once. Over Tor each connection has circuits of its own, so
//! a download runs at what several circuits carry together rather than what
//! one does, and a connection that fails or stalls hands what it had left to
//! a new one on new circuits.
//!
//! The first request asks for `bytes=0-`. When the server answers 206 with the
//! file's length, the first connection keeps reading from 0 and the rest is
//! split among the others; a connection that finishes its part takes half of
//! the largest part still left, so a slow circuit does not hold the download.
//! Every later answer must be the same file: the same length, and `If-Range`
//! carries the first answer's validator, so a file that changed comes back
//! whole (200) and the download fails rather than splice two versions. A
//! server that answers 200 is read in one stream.
//!
//! This is not the crawler, which reads only text and decides on the headers
//! before the body: a download happens only when a user asks for one.

use std::collections::VecDeque;
use std::fs::File;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;
use std::time::Duration;

use ::http::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HOST,
    HeaderName, HeaderValue, IF_RANGE, LAST_MODIFIED, LOCATION, RANGE, USER_AGENT,
};
use ::http::{HeaderMap, Request, Response};
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Body as _, Incoming};
use omnid_core::{Network, Source};
use rustls::pki_types::ServerName;
use tokio::sync::Notify;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

use crate::http::{self, Sender};
use crate::tor::Circuits;
use crate::{BoxError, FetchError, Gateway, HYPHANET_HOST, stream_target};

/// Connections one download uses at most.
pub const MAX_CONNECTIONS: usize = 16;
/// As many as [`FetchOptions`](crate::FetchOptions) follows by default.
const MAX_REDIRECTS: usize = 5;
/// Bytes received are written in blocks of up to this.
const WRITE_BLOCK: usize = 64 << 10;
/// Header values longer than these are malformed and taken as absent.
const MAX_LOCATION: usize = 8 << 10;
const MAX_DISPOSITION: usize = 2 << 10;
const MAX_VALIDATOR: usize = 256;
const MAX_CONTENT_TYPE: usize = 256;
/// Three 20-digit numbers and `bytes -/` fit in this.
const MAX_CONTENT_RANGE: usize = 80;
/// Names are cut to this many bytes (most file systems allow 255), keeping an
/// extension of up to [`MAX_EXTENSION`].
const MAX_FILENAME: usize = 200;
const MAX_EXTENSION: usize = 16;
/// The name of a file nothing names.
const FALLBACK_NAME: &str = "download";

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Connections at once, 1 to [`MAX_CONNECTIONS`].
    pub connections: usize,
    /// A file is not split into parts smaller than this.
    pub min_segment: u64,
    /// A larger file is refused before anything is written, and a stream of
    /// unknown length is stopped here.
    pub max_bytes: u64,
    /// Instead of the gateway's.
    pub user_agent: Option<String>,
    pub route: Route,
    /// What the caller already saw of the file. A server that answers with a
    /// different one fails the download with [`DownloadError::Mismatch`].
    pub expect: Option<Expected>,
    /// A connection that sends nothing for this long is dropped, and its part
    /// goes to a new one.
    pub stall_timeout: Duration,
    /// New connections one part gets, in a row, before the download fails. A
    /// connection that delivered `min_segment` or more before it failed does
    /// not count.
    pub max_retries: u32,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            connections: 4,
            min_segment: 1 << 20,
            max_bytes: 16 << 30,
            user_agent: None,
            route: Route::Direct,
            expect: None,
            stall_timeout: Duration::from_secs(30),
            max_retries: 4,
        }
    }
}

/// Which way a download's connections go.
#[derive(Debug, Clone, Default)]
pub enum Route {
    /// As [`Gateway::connect`] routes: the clearnet directly (or through Tor
    /// with `clearnet_via_tor`), onions through Tor, `.i2p` through i2pd,
    /// [`HYPHANET_HOST`] to the Hyphanet node.
    #[default]
    Direct,
    /// As [`Gateway::connect_browser`] routes, for a download the browser
    /// started on `site`: never directly. The first connection goes on the
    /// site's circuits, as the page did; the others on circuits of their own.
    Browser { site: String },
}

/// The file the caller saw, from the response headers it has.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    /// The file's length. Leave it out when that response had a
    /// Content-Encoding: its length was the encoded body's.
    pub length: Option<u64>,
    /// Compared weakly: `W/"x"` matches `"x"`.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// How far a download is, for another thread to read while it runs.
#[derive(Debug, Default)]
pub struct Progress {
    total: AtomicU64,
    done: AtomicU64,
    active: AtomicUsize,
    retries: AtomicU32,
}

impl Progress {
    /// The file's length; 0 until it is known, and for a stream that does not
    /// say.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Bytes received so far.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Connections fetching a part now.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Times a part went to a new connection after one failed or stalled.
    pub fn retries(&self) -> u32 {
        self.retries.load(Ordering::Relaxed)
    }

    fn reset(&self) {
        self.total.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.active.store(0, Ordering::Relaxed);
        self.retries.store(0, Ordering::Relaxed);
    }
}

/// A finished download.
#[derive(Debug, Clone)]
pub struct Download {
    /// Where the file came from, after redirects.
    pub url: Url,
    pub bytes: u64,
    /// Whether the server sent byte ranges: the file came in parts, and a
    /// part whose connection failed was resumed where it stopped.
    pub ranged: bool,
    /// The most connections open at once.
    pub connections: usize,
    pub retries: u32,
    pub content_type: Option<String>,
    /// A name to save it as, from Content-Disposition or the URL, made safe
    /// by [`sanitize_filename`].
    pub filename: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("cannot download this: {0}")]
    Unsupported(String),
    #[error("the server answered {0}")]
    Status(u16),
    /// The server's file is not the one [`DownloadOptions::expect`] describes:
    /// the caller can fetch it in a stream of its own instead.
    #[error("not the file expected: {0}")]
    Mismatch(String),
    /// The file is another one than when the download started; nothing of
    /// the new one was written.
    #[error("the file changed on the server during the download")]
    Changed,
    #[error("the file is larger than the limit of {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("bad response: {0}")]
    Protocol(String),
    #[error("nothing received for {0:?}")]
    Stalled(Duration),
    #[error("more than {MAX_REDIRECTS} redirects")]
    TooManyRedirects,
    #[error("cancelled")]
    Cancelled,
    #[error("writing the file: {0}")]
    Io(#[from] io::Error),
}

impl DownloadError {
    /// Whether a new connection, on new circuits, may do better.
    fn is_transient(&self) -> bool {
        match self {
            DownloadError::Fetch(FetchError::Transport { .. } | FetchError::Timeout(_)) | DownloadError::Stalled(_) => {
                true
            }
            DownloadError::Status(status) => is_transient_status(*status),
            _ => false,
        }
    }
}

fn is_transient_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

impl Gateway {
    /// Downloads `url` into `file`, which the caller opened for writing (not
    /// appending): in byte ranges over up to `opts.connections` connections
    /// when the server sends them, else in one stream. `progress` can be read
    /// from another thread meanwhile, and `cancel` stops the download. After
    /// an error the file is incomplete; nothing is written to it once this
    /// returns.
    pub async fn download(
        &self,
        url: &Url,
        file: &File,
        opts: &DownloadOptions,
        progress: &Progress,
        cancel: &CancellationToken,
    ) -> Result<Download, DownloadError> {
        let (host, _) = target(url)?;
        let user_agent = opts.user_agent.as_deref().unwrap_or(&self.cfg.user_agent);
        if HeaderValue::from_str(user_agent).is_err() {
            return Err(DownloadError::Unsupported(format!("the user agent {user_agent:?}")));
        }
        let network = stream_target(&host).1;
        let mut tls = crate::tls_config().map_err(|error| FetchError::Transport { network, error })?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        progress.reset();
        let job = Job {
            gateway: self,
            opts,
            progress,
            user_agent,
            tls: TlsConnector::from(Arc::new(tls)),
            file: Arc::new(file.try_clone()?),
            stop: cancel.child_token(),
            peak: AtomicUsize::new(0),
        };
        match job.run(url.clone()).await {
            Err(_) if cancel.is_cancelled() => Err(DownloadError::Cancelled),
            result => result,
        }
    }

    /// Where a redirect from `current` leads, resolved as [`Gateway::fetch`]
    /// resolves one: onto whatever network the target is on, a Hyphanet key to
    /// the node at [`HYPHANET_HOST`], and so are FProxy's absolute URLs.
    fn redirect(&self, current: &Url, location: &str) -> Option<Url> {
        let location = location.trim();
        if let Some((scheme, _)) = location.split_once(':')
            && (scheme.eq_ignore_ascii_case("freenet") || scheme.eq_ignore_ascii_case("hyphanet"))
        {
            let Ok(Source::Hyphanet(key)) = Source::parse(location) else { return None };
            return Url::parse(&format!("http://{HYPHANET_HOST}/{key}")).ok();
        }
        let location = match current.host_str() {
            Some(HYPHANET_HOST) => self
                .fproxy
                .urls()
                .find_map(|node| location.strip_prefix(node.as_str().trim_end_matches('/')))
                .unwrap_or(location),
            _ => location,
        };
        let next = current.join(location).ok()?;
        matches!(next.scheme(), "http" | "https").then_some(next)
    }
}

/// One download's settings and state, shared by its connections.
struct Job<'a> {
    gateway: &'a Gateway,
    opts: &'a DownloadOptions,
    progress: &'a Progress,
    user_agent: &'a str,
    tls: TlsConnector,
    file: Arc<File>,
    /// Cancelled with the caller's token, and when an error on one connection
    /// ends the download.
    stop: CancellationToken,
    /// The most connections open at once.
    peak: AtomicUsize,
}

/// An answer's head, and the connection its body comes on.
struct Answer {
    sender: Sender,
    response: Response<Incoming>,
}

impl Job<'_> {
    async fn run(&self, url: Url) -> Result<Download, DownloadError> {
        let (url, answer) = self.probe(url).await?;
        let headers = answer.response.headers();
        let content_type = value(headers, CONTENT_TYPE, MAX_CONTENT_TYPE);
        let filename = suggested_filename(headers, &url);
        let validators = Validators {
            etag: value(headers, ETAG, MAX_VALIDATOR),
            last_modified: value(headers, LAST_MODIFIED, MAX_VALIDATOR),
        };
        let (bytes, ranged) = match answer.response.status().as_u16() {
            206 => {
                let total = match content_range(headers)? {
                    ContentRange::Bytes { first: 0, total: Some(total), .. } => total,
                    range => return Err(DownloadError::Protocol(format!("bytes 0- answered with {range:?}"))),
                };
                self.check(Some(total), &validators)?;
                (Box::pin(self.ranged(&url, answer, total, validators)).await?, true)
            }
            200 => {
                let length = value(headers, CONTENT_LENGTH, 20).and_then(|length| number(&length));
                self.check(length, &validators)?;
                let bytes = Box::pin(self.single(&url, answer, length)).await?;
                self.check(Some(bytes), &Validators::default())?;
                (bytes, false)
            }
            // `bytes=0-` of an empty file.
            416 if content_range(headers).ok() == Some(ContentRange::Unsatisfied { total: 0 }) => {
                self.check(Some(0), &validators)?;
                self.set_len(0).await?;
                (0, false)
            }
            status => return Err(DownloadError::Status(status)),
        };
        Ok(Download {
            url,
            bytes,
            ranged,
            connections: self.peak.load(Ordering::Relaxed),
            retries: self.progress.retries(),
            content_type,
            filename,
        })
    }

    /// The first request, `bytes=0-`, following redirects, and retried on new
    /// circuits when a new connection may do better.
    async fn probe(&self, mut url: Url) -> Result<(Url, Answer), DownloadError> {
        let mut circuits = match &self.opts.route {
            Route::Browser { site } => Circuits::Site(site),
            Route::Direct => Circuits::own(),
        };
        let (mut redirects, mut failures) = (0, 0);
        loop {
            let attempt = async {
                let mut sender = self.open(&url, circuits).await?;
                let response = self.send(&mut sender, &url, 0, None, None).await?;
                match response.status().as_u16() {
                    status if is_transient_status(status) => Err(DownloadError::Status(status)),
                    _ => Ok(Answer { sender, response }),
                }
            };
            let answer = match attempt.await {
                Ok(answer) => answer,
                Err(error) if error.is_transient() && failures < self.opts.max_retries => {
                    tracing::debug!(%url, "download: {error}; trying again on a new connection");
                    failures += 1;
                    self.progress.retries.fetch_add(1, Ordering::Relaxed);
                    circuits = Circuits::own();
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !http::is_redirect(answer.response.status().as_u16()) {
                return Ok((url, answer));
            }
            let location = value(answer.response.headers(), LOCATION, MAX_LOCATION)
                .ok_or_else(|| DownloadError::Protocol("a redirect without a usable Location".into()))?;
            if redirects == MAX_REDIRECTS {
                return Err(DownloadError::TooManyRedirects);
            }
            redirects += 1;
            url = self.gateway.redirect(&url, &location).ok_or(FetchError::BadRedirect(location))?;
        }
    }

    /// Whether the server's file is the one the caller expects.
    fn check(&self, length: Option<u64>, validators: &Validators) -> Result<(), DownloadError> {
        let Some(expect) = &self.opts.expect else { return Ok(()) };
        let mismatch = |what: String| Err(DownloadError::Mismatch(what));
        if let (Some(want), Some(got)) = (expect.length, length)
            && want != got
        {
            return mismatch(format!("{got} bytes, not {want}"));
        }
        if let (Some(want), Some(got)) = (&expect.etag, &validators.etag)
            && opaque(want) != opaque(got)
        {
            return mismatch(format!("ETag {got}, not {want}"));
        }
        if let (Some(want), Some(got)) = (&expect.last_modified, &validators.last_modified)
            && want.trim() != got
        {
            return mismatch(format!("Last-Modified {got}, not {want}"));
        }
        Ok(())
    }

    /// A 200: the body in one stream, written in order.
    async fn single(&self, url: &Url, answer: Answer, length: Option<u64>) -> Result<u64, DownloadError> {
        let max = self.opts.max_bytes;
        if length.is_some_and(|length| length > max) {
            return Err(DownloadError::TooLarge { limit: max });
        }
        self.progress.total.store(length.unwrap_or(0), Ordering::Relaxed);
        self.set_len(0).await?;
        let network = network(url);
        let Answer { sender: _connection, response } = answer;
        let mut body = response.into_body();
        let mut buf = BytesMut::new();
        let mut written = 0;
        self.connected();
        let streamed = async {
            while let Some(data) = self.next_data(&mut body, network).await? {
                if written + (buf.len() + data.len()) as u64 > max {
                    return Err(DownloadError::TooLarge { limit: max });
                }
                self.progress.done.fetch_add(data.len() as u64, Ordering::Relaxed);
                buf.extend_from_slice(&data);
                if buf.len() >= WRITE_BLOCK {
                    written += self.write_block(&mut buf, written).await?;
                }
            }
            written += self.write_block(&mut buf, written).await?;
            Ok(written)
        }
        .await;
        self.disconnected();
        streamed
    }

    /// A 206: the file in parts, over up to `opts.connections` connections,
    /// the first of them already on `answer`.
    async fn ranged(
        &self,
        url: &Url,
        answer: Answer,
        total: u64,
        validators: Validators,
    ) -> Result<u64, DownloadError> {
        if total > self.opts.max_bytes {
            return Err(DownloadError::TooLarge { limit: self.opts.max_bytes });
        }
        self.progress.total.store(total, Ordering::Relaxed);
        self.set_len(total).await?;
        let min_segment = self.opts.min_segment.max(1);
        let wanted = self.opts.connections.clamp(1, MAX_CONNECTIONS) as u64;
        let connections = (total / min_segment).clamp(1, wanted) as usize;
        let parts = Parts {
            job: self,
            url,
            network: network(url),
            total,
            if_range: validators.if_range(),
            validators,
            plan: Mutex::new(Plan::new(total, connections, min_segment)),
            changed: Notify::new(),
        };
        let mut first = Some(answer);
        join_all((0..connections).map(|slot| parts.worker(slot, if slot == 0 { first.take() } else { None }))).await;
        let plan = parts.plan.into_inner().expect("plan lock poisoned");
        if let Some(error) = plan.error {
            return Err(error);
        }
        if self.stop.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        if plan.written != total {
            return Err(DownloadError::Protocol(format!("{} of {total} bytes written", plan.written)));
        }
        Ok(total)
    }

    /// A new HTTP/1.1 connection to `url`'s host, routed as the options say.
    async fn open(&self, url: &Url, circuits: Circuits<'_>) -> Result<Sender, DownloadError> {
        let (host, port) = target(url)?;
        // Boxed: on top of arti's, the download's future would nest deeper
        // than the compiler lays types out.
        let connect: Pin<Box<dyn Future<Output = Result<_, FetchError>> + Send + '_>> = match &self.opts.route {
            Route::Direct => Box::pin(self.gateway.connect_on(&host, port, circuits)),
            Route::Browser { .. } => Box::pin(self.gateway.connect_browser_on(&host, port, circuits)),
        };
        let stream = self.stoppable(async { Ok(connect.await?) }).await?;
        let network = stream_target(&host).1;
        let transport = |error: BoxError| DownloadError::Fetch(FetchError::Transport { network, error });
        self.timed(async {
            if url.scheme() == "https" {
                let name = ServerName::try_from(host.clone()).map_err(|e| transport(e.into()))?;
                let stream = self.tls.connect(name, stream).await.map_err(|e| transport(e.into()))?;
                http::handshake(stream).await.map_err(transport)
            } else {
                http::handshake(stream).await.map_err(transport)
            }
        })
        .await
    }

    /// A GET for the bytes from `start` to `last` (or the end), and the
    /// answer's head.
    async fn send(
        &self,
        sender: &mut Sender,
        url: &Url,
        start: u64,
        last: Option<u64>,
        if_range: Option<&str>,
    ) -> Result<Response<Incoming>, DownloadError> {
        let host = url.host_str().unwrap_or_default();
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        let range = match last {
            Some(last) => format!("bytes={start}-{last}"),
            None => format!("bytes={start}-"),
        };
        let mut request = Request::get(&url[url::Position::BeforePath..url::Position::AfterQuery])
            .header(HOST, authority)
            .header(USER_AGENT, self.user_agent)
            .header(ACCEPT, "*/*")
            // Ranges count the bytes as sent: a compressed body's are not the
            // file's.
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, range);
        if let Some(validator) = if_range {
            request = request.header(IF_RANGE, validator);
        }
        let request =
            request.body(Empty::<Bytes>::new()).map_err(|e| DownloadError::Unsupported(format!("{url}: {e}")))?;
        let network = network(url);
        let transport =
            |error: hyper::Error| DownloadError::Fetch(FetchError::Transport { network, error: error.into() });
        self.timed(async {
            sender.ready().await.map_err(transport)?;
            sender.send_request(request).await.map_err(transport)
        })
        .await
    }

    /// A body's next bytes; `None` at its end.
    async fn next_data(&self, body: &mut Incoming, network: Network) -> Result<Option<Bytes>, DownloadError> {
        loop {
            match self.timed(async { Ok(body.frame().await) }).await? {
                None => return Ok(None),
                Some(Err(error)) => return Err(FetchError::Transport { network, error: error.into() }.into()),
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data()
                        && !data.is_empty()
                    {
                        return Ok(Some(data));
                    }
                }
            }
        }
    }

    /// `step`, given up when the download stops.
    async fn stoppable<T>(&self, step: impl Future<Output = Result<T, DownloadError>>) -> Result<T, DownloadError> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(DownloadError::Cancelled),
            result = step => result,
        }
    }

    /// `step`, given up when the download stops or after the stall timeout.
    async fn timed<T>(&self, step: impl Future<Output = Result<T, DownloadError>>) -> Result<T, DownloadError> {
        let stall = self.opts.stall_timeout;
        let step = async { tokio::time::timeout(stall, step).await.unwrap_or(Err(DownloadError::Stalled(stall))) };
        self.stoppable(step).await
    }

    /// Writes what `buf` holds at `offset` and empties it; says how much.
    async fn write_block(&self, buf: &mut BytesMut, offset: u64) -> Result<u64, DownloadError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let data = buf.split().freeze();
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || write_all_at(&file, &data, offset).map(|()| data.len() as u64))
            .await
            .map_err(io::Error::other)?
            .map_err(DownloadError::Io)
    }

    async fn set_len(&self, len: u64) -> Result<(), DownloadError> {
        let file = self.file.clone();
        Ok(tokio::task::spawn_blocking(move || file.set_len(len)).await.map_err(io::Error::other)??)
    }

    fn connected(&self) {
        let active = self.progress.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
    }

    fn disconnected(&self) {
        self.progress.active.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(unix)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, data, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        let written = file.seek_write(data, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[written..];
        offset += written as u64;
    }
    Ok(())
}

/// The host to connect to (IPv6 without brackets) and the port, for http and
/// https URLs.
fn target(url: &Url) -> Result<(String, u16), DownloadError> {
    let unsupported = || DownloadError::Unsupported(format!("{url}: only http and https addresses"));
    if !matches!(url.scheme(), "http" | "https") {
        return Err(unsupported());
    }
    let host = match url.host().ok_or_else(unsupported)? {
        Host::Domain(domain) => domain.to_owned(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    Ok((host, url.port_or_known_default().ok_or_else(unsupported)?))
}

fn network(url: &Url) -> Network {
    stream_target(url.host_str().unwrap_or_default()).1
}

/// What identifies the version of the file the first answer was.
#[derive(Default)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Validators {
    /// For `If-Range`: a strong ETag, else the date. A weak ETag cannot be
    /// used there.
    fn if_range(&self) -> Option<String> {
        self.etag.clone().filter(|tag| tag.starts_with('"')).or_else(|| self.last_modified.clone())
    }

    /// Whether a later answer's validators are this version's.
    fn matches(&self, headers: &HeaderMap) -> bool {
        let same = |mine: &Option<String>, name| match (mine, value(headers, name, MAX_VALIDATOR)) {
            (Some(mine), Some(theirs)) => opaque(mine) == opaque(&theirs),
            _ => true,
        };
        same(&self.etag, ETAG) && same(&self.last_modified, LAST_MODIFIED)
    }
}

/// An entity tag without its weakness, for the weak comparison (RFC 9110
/// 8.8.3.2).
fn opaque(tag: &str) -> &str {
    let tag = tag.trim();
    tag.strip_prefix("W/").unwrap_or(tag)
}

/// A ranged download's shared state.
struct Parts<'a> {
    job: &'a Job<'a>,
    url: &'a Url,
    network: Network,
    total: u64,
    validators: Validators,
    if_range: Option<String>,
    plan: Mutex<Plan>,
    /// Woken when a part is put back or the last one is done, for connections
    /// waiting for one to take.
    changed: Notify,
}

impl Parts<'_> {
    /// One connection's work: its part, then whatever there is to take, until
    /// the file is done or the download stops.
    async fn worker(&self, slot: usize, mut first: Option<Answer>) {
        let mut kept = None;
        loop {
            if first.is_none() && !self.take(slot).await {
                return;
            }
            self.job.connected();
            let fetched = self.fetch(slot, first.take(), &mut kept).await;
            self.job.disconnected();
            let error = match fetched {
                Ok(()) => {
                    self.changed.notify_waiters();
                    continue;
                }
                Err(DownloadError::Cancelled) => return,
                Err(error) => error,
            };
            kept = None;
            let mut plan = self.plan();
            let retry = match plan.release(slot) {
                Some(part) if error.is_transient() && part.retries <= self.job.opts.max_retries => Some(part),
                None if error.is_transient() => None,
                _ => {
                    plan.error.get_or_insert(error);
                    drop(plan);
                    self.job.stop.cancel();
                    return;
                }
            };
            if let Some(part) = retry {
                tracing::debug!(url = %self.url, start = part.next, "download: {error}; the rest of the part goes to a new connection");
                plan.queue.push_back(part);
                self.job.progress.retries.fetch_add(1, Ordering::Relaxed);
            }
            drop(plan);
            self.changed.notify_waiters();
        }
    }

    /// Gives `slot` a part: one that was put back, else half of the largest
    /// part another connection is on. With none to take while others are
    /// still on theirs, waits: one of them may fail and be put back. False
    /// once the file is done or the download stopped.
    async fn take(&self, slot: usize) -> bool {
        loop {
            let changed = self.changed.notified();
            let take = self.plan().take(slot);
            match take {
                Take::Part => return true,
                Take::Done => return false,
                Take::Wait => {}
            }
            tokio::select! {
                () = changed => {}
                () = self.job.stop.cancelled() => return false,
            }
        }
    }

    /// Fetches the part `slot` is on: on `first` (the first answer, already
    /// reading from 0), on the connection kept from its last part, or on a new
    /// one with circuits of its own. Keeps the connection when the part ended
    /// with its answer.
    async fn fetch(&self, slot: usize, first: Option<Answer>, kept: &mut Option<Sender>) -> Result<(), DownloadError> {
        let (sender, response) = match first {
            Some(Answer { sender, response }) => (sender, response),
            None => self.request(slot, kept).await?,
        };
        let reusable = !closes(response.headers());
        let mut body = response.into_body();
        let mut buf = BytesMut::new();
        let mut at = 0;
        let ended = loop {
            let data = match self.job.next_data(&mut body, self.network).await {
                Ok(Some(data)) => data,
                Ok(None) => {
                    let error = "the connection ended before its part did".into();
                    break Err(FetchError::Transport { network: self.network, error }.into());
                }
                Err(error) => break Err(error),
            };
            let (offset, take, done) = self.plan().claim(slot, data.len() as u64);
            if buf.is_empty() {
                at = offset;
            }
            buf.extend_from_slice(&data[..take as usize]);
            self.job.progress.done.fetch_add(take, Ordering::Relaxed);
            if done {
                break Ok(());
            }
            if buf.len() >= WRITE_BLOCK {
                self.write(&mut buf, &mut at).await?;
            }
        };
        // What was received is the file's, whatever came after it.
        if !matches!(ended, Err(DownloadError::Cancelled)) {
            self.write(&mut buf, &mut at).await?;
        }
        ended?;
        if reusable
            && body.is_end_stream()
            && matches!(self.job.timed(async { Ok(body.frame().await) }).await, Ok(None))
        {
            *kept = Some(sender);
        }
        self.plan().finish(slot);
        Ok(())
    }

    /// Asks for what `slot`'s part has left, and checks the answer is that
    /// part of the same file.
    async fn request(
        &self,
        slot: usize,
        kept: &mut Option<Sender>,
    ) -> Result<(Sender, Response<Incoming>), DownloadError> {
        let (start, end) = self.plan().span(slot);
        let (job, url, last, if_range) = (self.job, self.url, end - 1, self.if_range.as_deref());
        // The server may have closed a kept connection since: then a new one,
        // which is no retry.
        let reused = match kept.take() {
            Some(mut sender) if !sender.is_closed() => {
                match job.send(&mut sender, url, start, Some(last), if_range).await {
                    Ok(response) => Some((sender, response)),
                    Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let (sender, response) = match reused {
            Some(answer) => answer,
            None => {
                let mut sender = job.open(url, Circuits::own()).await?;
                let response = job.send(&mut sender, url, start, Some(last), if_range).await?;
                (sender, response)
            }
        };
        self.check(&response, start, last)?;
        Ok((sender, response))
    }

    fn check(&self, response: &Response<Incoming>, start: u64, last: u64) -> Result<(), DownloadError> {
        match response.status().as_u16() {
            206 => {}
            // If-Range did not match: the file is another one now.
            200 if self.if_range.is_some() => return Err(DownloadError::Changed),
            200 => return Err(DownloadError::Protocol("a range was answered with the whole file".into())),
            status => return Err(DownloadError::Status(status)),
        }
        let ContentRange::Bytes { first, last: sent_last, total } = content_range(response.headers())? else {
            return Err(DownloadError::Protocol("a range was answered with none".into()));
        };
        if total != Some(self.total) || !self.validators.matches(response.headers()) {
            return Err(DownloadError::Changed);
        }
        if first != start || sent_last > last {
            return Err(DownloadError::Protocol(format!("asked for bytes {start}-{last}, sent {first}-{sent_last}")));
        }
        Ok(())
    }

    /// Writes what `buf` holds, at `at`, and moves `at` past it.
    async fn write(&self, buf: &mut BytesMut, at: &mut u64) -> Result<(), DownloadError> {
        let written = self.job.write_block(buf, *at).await?;
        *at += written;
        self.plan().written += written;
        Ok(())
    }

    fn plan(&self) -> MutexGuard<'_, Plan> {
        self.plan.lock().expect("plan lock poisoned")
    }
}

/// Which bytes are whose. Each byte of the file is in one place: written or
/// being written, in a connection's part (`next..end`), or in `queue`.
struct Plan {
    min_segment: u64,
    /// Parts no connection is on.
    queue: VecDeque<Part>,
    /// The part each connection is on, by slot.
    active: Vec<Option<Part>>,
    written: u64,
    /// The error that ended the download.
    error: Option<DownloadError>,
}

/// Bytes `next..end`, of a part that started at `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Part {
    start: u64,
    next: u64,
    end: u64,
    /// Connections that failed on it in a row.
    retries: u32,
}

impl Part {
    fn new(start: u64, end: u64) -> Self {
        Self { start, next: start, end, retries: 0 }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Take {
    Part,
    Wait,
    Done,
}

impl Plan {
    /// `total` bytes in `connections` equal parts, the first one on slot 0.
    fn new(total: u64, connections: usize, min_segment: u64) -> Self {
        let bound = |i: usize| (u128::from(total) * i as u128 / connections as u128) as u64;
        let mut active = vec![None; connections];
        active[0] = Some(Part::new(0, bound(1)));
        let queue = (1..connections).map(|i| Part::new(bound(i), bound(i + 1))).collect();
        Self { min_segment, queue, active, written: 0, error: None }
    }

    /// Puts `slot` on a part that was put back, else on half of the largest
    /// part another slot is on, if each half is at least `min_segment`.
    fn take(&mut self, slot: usize) -> Take {
        if let Some(part) = self.queue.pop_front() {
            self.active[slot] = Some(part);
            return Take::Part;
        }
        let largest = self
            .active
            .iter()
            .enumerate()
            .filter_map(|(i, part)| Some((i, part.as_ref()?)))
            .max_by_key(|(_, part)| part.end - part.next);
        let Some((victim, part)) = largest else { return Take::Done };
        let left = part.end - part.next;
        if left < self.min_segment.saturating_mul(2) {
            return Take::Wait;
        }
        let middle = part.next + left / 2;
        let stolen = Part::new(middle, part.end);
        self.active[victim].as_mut().expect("found above").end = middle;
        self.active[slot] = Some(stolen);
        Take::Part
    }

    /// Takes up to `len` bytes `slot` received, at where its part is: no more
    /// than the part has left, which another slot may have cut short. Says
    /// where they go, how many, and whether the part is done.
    fn claim(&mut self, slot: usize, len: u64) -> (u64, u64, bool) {
        let part = self.active[slot].as_mut().expect("only a slot on a part receives");
        let take = len.min(part.end - part.next);
        let at = part.next;
        part.next += take;
        (at, take, part.next == part.end)
    }

    /// Where `slot`'s part has its next byte, and where it ends.
    fn span(&self, slot: usize) -> (u64, u64) {
        let part = self.active[slot].as_ref().expect("only a slot on a part asks");
        (part.next, part.end)
    }

    fn finish(&mut self, slot: usize) {
        self.active[slot] = None;
    }

    /// Takes `slot` off its part after its connection failed, and gives back
    /// what the part has left, if anything. That counts as a retry, unless the
    /// connection delivered `min_segment` first.
    fn release(&mut self, slot: usize) -> Option<Part> {
        let part = self.active[slot].take()?;
        if part.next == part.end {
            return None;
        }
        let retries = if part.next - part.start >= self.min_segment { 0 } else { part.retries + 1 };
        Some(Part { start: part.next, next: part.next, end: part.end, retries })
    }
}

/// Runs `futures` together on this task until every one has finished.
async fn join_all<F: Future<Output = ()>>(futures: impl IntoIterator<Item = F>) {
    let mut running: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    std::future::poll_fn(move |cx| {
        running.retain_mut(|future| future.as_mut().poll(cx).is_pending());
        if running.is_empty() { Poll::Ready(()) } else { Poll::Pending }
    })
    .await
}

/// A header's value, trimmed, when it is text of at most `max` bytes.
fn value(headers: &HeaderMap, name: HeaderName, max: usize) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty() && value.len() <= max).then(|| value.to_owned())
}

fn closes(headers: &HeaderMap) -> bool {
    headers.get_all(CONNECTION).iter().any(|value| {
        value.to_str().is_ok_and(|value| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("close")))
    })
}

/// A `Content-Range` (RFC 9110 14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentRange {
    /// `bytes FIRST-LAST/TOTAL`, `TOTAL` unknown when `*`.
    Bytes { first: u64, last: u64, total: Option<u64> },
    /// `bytes */TOTAL`, for a range that could not be satisfied.
    Unsatisfied { total: u64 },
}

/// The one `Content-Range` an answer has.
fn content_range(headers: &HeaderMap) -> Result<ContentRange, DownloadError> {
    let mut values = headers.get_all(CONTENT_RANGE).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(DownloadError::Protocol("not one Content-Range".into()));
    };
    value.to_str().ok().and_then(parse_content_range).ok_or_else(|| {
        let shown = String::from_utf8_lossy(&value.as_bytes()[..value.len().min(MAX_CONTENT_RANGE)]).into_owned();
        DownloadError::Protocol(format!("a malformed Content-Range {shown:?}"))
    })
}

/// Strict: one space after `bytes`, digits only, `FIRST <= LAST < TOTAL`.
fn parse_content_range(value: &str) -> Option<ContentRange> {
    if value.len() > MAX_CONTENT_RANGE {
        return None;
    }
    let (unit, rest) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = rest.split_once('/')?;
    if range == "*" {
        return Some(ContentRange::Unsatisfied { total: number(total)? });
    }
    let (first, last) = range.split_once('-')?;
    let (first, last) = (number(first)?, number(last)?);
    let total = if total == "*" { None } else { Some(number(total)?) };
    (first <= last && total.is_none_or(|total| last < total)).then_some(ContentRange::Bytes { first, last, total })
}

/// A decimal number of ASCII digits only (no sign, no spaces) that fits a
/// u64.
fn number(digits: &str) -> Option<u64> {
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// What to save a download as: the name `Content-Disposition` gives, else the
/// URL's last path segment, made safe by [`sanitize_filename`].
fn suggested_filename(headers: &HeaderMap, url: &Url) -> String {
    let given = headers
        .get(CONTENT_DISPOSITION)
        .filter(|value| value.len() <= MAX_DISPOSITION)
        .and_then(|value| disposition_filename(&String::from_utf8_lossy(value.as_bytes())));
    sanitize_filename(&given.or_else(|| url_filename(url)).unwrap_or_default())
}

/// The name a `Content-Disposition` value gives (RFC 6266): `filename*`
/// (RFC 8187: UTF-8 or ISO-8859-1, percent-encoded) over `filename`, quoted
/// or not. Not yet safe to use: see [`sanitize_filename`].
fn disposition_filename(value: &str) -> Option<String> {
    if value.len() > MAX_DISPOSITION {
        return None;
    }
    let (mut plain, mut extended) = (None, None);
    for (name, value) in parameters(value) {
        if name.eq_ignore_ascii_case("filename*") {
            extended = extended.or_else(|| ext_value(&value));
        } else if name.eq_ignore_ascii_case("filename") {
            plain = plain.or(Some(value));
        }
    }
    extended.or(plain).filter(|name| !name.is_empty())
}

/// The `name=value` parameters after the first `;`, values unquoted.
fn parameters(header: &str) -> Vec<(&str, String)> {
    let mut parameters = Vec::new();
    let Some((_, mut rest)) = header.split_once(';') else { return parameters };
    while let Some((name, after)) = rest.split_once('=') {
        // Whatever came before a `;` in it was no parameter.
        let name = name.rsplit(';').next().unwrap_or_default().trim();
        let after = after.trim_start();
        let (value, tail) = match after.strip_prefix('"') {
            Some(quoted) => unquote(quoted),
            None => match after.split_once(';') {
                Some((value, tail)) => (value.trim().to_owned(), tail),
                None => (after.trim().to_owned(), ""),
            },
        };
        parameters.push((name, value));
        rest = tail;
    }
    parameters
}

/// A quoted string's text up to its closing quote (or the end), and what
/// follows the next `;` after it.
fn unquote(quoted: &str) -> (String, &str) {
    let mut text = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => text.extend(chars.next().map(|(_, escaped)| escaped)),
            '"' => return (text, quoted[i + 1..].split_once(';').map_or("", |(_, tail)| tail)),
            c => text.push(c),
        }
    }
    (text, "")
}

/// `charset'language'percent-encoded` (RFC 8187).
fn ext_value(value: &str) -> Option<String> {
    let (charset, rest) = value.split_once('\'')?;
    let (_language, encoded) = rest.split_once('\'')?;
    let bytes = percent_decode(encoded)?;
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}

/// `None` for a `%` not followed by two hex digits.
fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let hex = |b: Option<u8>| char::from(b?).to_digit(16).map(|digit| digit as u8);
    let mut bytes = text.bytes();
    let mut decoded = Vec::with_capacity(text.len());
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let (high, low) = (hex(bytes.next())?, hex(bytes.next())?);
            decoded.push((high << 4) | low);
        } else {
            decoded.push(b);
        }
    }
    Some(decoded)
}

/// The URL's last non-empty path segment, decoded.
fn url_filename(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.rev().find(|segment| !segment.is_empty())?;
    let bytes = percent_decode(segment).unwrap_or_else(|| segment.as_bytes().to_vec());
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A name to create a file with, whatever the server said: the last
/// component of `name` (never a path); no control characters and no
/// bidirectional controls, which can make `exe.txt` show as `txt.exe`;
/// `<>:"|?*`, which some file systems refuse, as `_`; no leading or trailing
/// dots or spaces; at most 200 bytes, keeping the extension; never empty.
pub fn sanitize_filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = name
        .chars()
        .filter(|&c| !is_hidden(c))
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let trim = |name: &str| name.trim_matches(|c: char| c.is_whitespace() || c == '.').to_owned();
    let mut name = trim(&cleaned);
    if name.len() > MAX_FILENAME {
        let extension = name.rfind('.').map(|dot| name[dot..].to_owned()).filter(|ext| ext.len() <= MAX_EXTENSION);
        let extension = extension.unwrap_or_default();
        let mut cut = MAX_FILENAME - extension.len();
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name = trim(&name[..cut]) + &extension;
    }
    if name.is_empty() { FALLBACK_NAME.to_owned() } else { name }
}

fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_ranges_strictly() {
        let bytes = |first, last, total| Some(ContentRange::Bytes { first, last, total });
        assert_eq!(parse_content_range("bytes 0-499/1234"), bytes(0, 499, Some(1234)));
        assert_eq!(parse_content_range(" Bytes 5-5/6 "), bytes(5, 5, Some(6)));
        assert_eq!(parse_content_range("bytes 0-99/*"), bytes(0, 99, None));
        assert_eq!(parse_content_range("bytes */0"), Some(ContentRange::Unsatisfied { total: 0 }));
        let max = u64::MAX;
        assert_eq!(parse_content_range(&format!("bytes 0-{}/{max}", max - 1)), bytes(0, max - 1, Some(max)));
        for bad in [
            "",
            "bytes",
            "bytes 0-499",
            "items 0-499/1234",
            "bytes  0-499/1234",
            "bytes +0-499/1234",
            "bytes 0-+499/1234",
            "bytes 0 -499/1234",
            "bytes 0-499/ 1234",
            "bytes -499/1234",
            "bytes 0-/1234",
            "bytes 500-499/1234",
            "bytes 0-1234/1234",
            "bytes 0-99/50",
            "bytes 0x10-0x20/100",
            "bytes 0-1/18446744073709551616",
            "bytes */*",
            "bytes 0-1,2-3/10",
        ] {
            assert_eq!(parse_content_range(bad), None, "{bad:?}");
        }
        let long = format!("bytes 0-1/{}", "0".repeat(MAX_CONTENT_RANGE));
        assert_eq!(parse_content_range(&long), None, "longer than the cap");
        assert_eq!(number(&"9".repeat(21)), None);
    }

    #[test]
    fn reads_the_name_content_disposition_gives() {
        let name = disposition_filename;
        assert_eq!(name("attachment; filename=report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(
            name("attachment; FileName=\"a \\\"quoted\\\" name.txt\"").as_deref(),
            Some("a \"quoted\" name.txt")
        );
        assert_eq!(name("attachment; filename=\"semi;colon.txt\"; size=3").as_deref(), Some("semi;colon.txt"));
        assert_eq!(
            name("attachment; filename=\"plain.txt\"; filename*=UTF-8''%E2%82%AC%20rates.txt").as_deref(),
            Some("€ rates.txt"),
            "filename* wins, wherever it is"
        );
        assert_eq!(
            name("attachment; filename*=utf-8'en'na%C3%AFve.txt; filename=naive.txt").as_deref(),
            Some("naïve.txt")
        );
        assert_eq!(name("attachment; filename*=ISO-8859-1''caf%E9.txt").as_deref(), Some("café.txt"));
        assert_eq!(
            name("attachment; filename*=UTF-8''bad%ZZ.txt; filename=fallback.txt").as_deref(),
            Some("fallback.txt"),
            "a broken filename* falls back"
        );
        assert_eq!(name("attachment; filename*=KOI8-R''x.txt").as_deref(), None);
        assert_eq!(name("attachment; junk; filename=x.bin").as_deref(), Some("x.bin"));
        assert_eq!(name("attachment; filename=\"unterminated.txt").as_deref(), Some("unterminated.txt"));
        assert_eq!(name("inline").as_deref(), None);
        assert_eq!(name("attachment; filename=\"\"").as_deref(), None);
        assert_eq!(name(&format!("attachment; filename={}", "a".repeat(MAX_DISPOSITION))), None, "over the cap");
        // Every byte of a long, hostile value is looked at once.
        let many = "; a=\"\\".repeat(MAX_DISPOSITION / 8);
        assert_eq!(name(&format!("attachment{many}")), None);
    }

    #[test]
    fn falls_back_to_the_url_for_a_name() {
        let headers = HeaderMap::new();
        let name = |url: &str| suggested_filename(&headers, &Url::parse(url).unwrap());
        assert_eq!(name("https://example.com/files/r%C3%A9sum%C3%A9.pdf?x=1"), "résumé.pdf");
        assert_eq!(name("https://example.com/dir/"), "dir");
        assert_eq!(name("https://example.com/"), FALLBACK_NAME);
        assert_eq!(name("https://example.com/a%2F..%2F..%2Fetc%2Fpasswd"), "passwd");
        assert_eq!(name("http://hyphanet/CHK@abc,def,AAMC--8/song.ogg"), "song.ogg");
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"../../.bashrc\""));
        assert_eq!(suggested_filename(&headers, &Url::parse("https://example.com/x.bin").unwrap()), "bashrc");
    }

    #[test]
    fn sanitizes_names_into_one_safe_file_name() {
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_filename("...hidden"), "hidden");
        assert_eq!(sanitize_filename(" .config. "), "config");
        assert_eq!(sanitize_filename("a\u{0}b\nc\u{7f}d\u{85}e.txt"), "abcde.txt");
        assert_eq!(sanitize_filename("invoice\u{202e}fdp.exe"), "invoicefdp.exe", "no right-to-left override");
        assert_eq!(sanitize_filename("what? <a|b>: \"*\".txt"), "what_ _a_b__ ___.txt");
        assert_eq!(sanitize_filename("photo 日本.jpg"), "photo 日本.jpg");
        for empty in ["", ".", "..", "/", "a/", "\u{1}\u{2}", " . "] {
            assert_eq!(sanitize_filename(empty), FALLBACK_NAME, "{empty:?}");
        }

        let long = sanitize_filename(&format!("{}.tar.gz", "x".repeat(10_000)));
        assert_eq!(long.len(), MAX_FILENAME);
        assert!(long.ends_with("x.gz"), "the extension is kept: {long}");
        let wide = sanitize_filename(&format!("{}.txt", "日".repeat(1_000)));
        assert!(wide.len() <= MAX_FILENAME && wide.ends_with("日.txt"), "cut on a character boundary");
        let no_extension = sanitize_filename(&"y".repeat(300));
        assert_eq!(no_extension, "y".repeat(MAX_FILENAME));
        let odd_extension = sanitize_filename(&format!("a.{}", "z".repeat(300)));
        assert_eq!(odd_extension.len(), MAX_FILENAME, "an extension too long to be one is cut too");
    }

    #[test]
    fn splits_evenly_and_takes_half_of_the_largest_part() {
        let mut plan = Plan::new(1_000, 4, 100);
        assert_eq!(plan.active[0], Some(Part::new(0, 250)));
        assert_eq!(plan.queue, [Part::new(250, 500), Part::new(500, 750), Part::new(750, 1_000)]);
        for slot in 1..4 {
            assert_eq!(plan.take(slot), Take::Part);
        }
        assert_eq!(plan.claim(1, 300), (250, 250, true), "no more than the part");
        plan.finish(1);
        plan.claim(0, 10);
        plan.claim(2, 50);
        assert_eq!(plan.take(1), Take::Part);
        assert_eq!(plan.active[3], Some(Part::new(750, 875)), "the largest part left was slot 3's");
        assert_eq!(plan.active[1], Some(Part::new(875, 1_000)));
        assert_eq!(plan.claim(3, 200), (750, 125, true), "cut short at the new end");
    }

    #[test]
    fn does_not_split_below_the_minimum_and_waits_for_the_others() {
        let mut plan = Plan::new(400, 2, 100);
        assert_eq!(plan.take(1), Take::Part);
        assert_eq!(plan.claim(1, 200), (200, 200, true));
        plan.finish(1);
        plan.claim(0, 50);
        assert_eq!(plan.take(1), Take::Wait, "150 left: two halves would be under 100");
        assert_eq!(plan.claim(0, 150), (50, 150, true));
        plan.finish(0);
        assert_eq!(plan.take(1), Take::Done);
    }

    #[test]
    fn a_failed_part_goes_back_with_what_it_had_left() {
        let mut plan = Plan::new(1_000, 1, 100);
        plan.claim(0, 50);
        let part = plan.release(0).unwrap();
        assert_eq!(part, Part { start: 50, next: 50, end: 1_000, retries: 1 });
        plan.queue.push_back(part);
        assert_eq!(plan.take(0), Take::Part);
        plan.claim(0, 10);
        assert_eq!(plan.release(0).unwrap().retries, 2, "failing again without progress");
        let mut plan = Plan::new(1_000, 1, 100);
        plan.active[0].as_mut().unwrap().retries = 3;
        plan.claim(0, 100);
        assert_eq!(plan.release(0).unwrap().retries, 0, "a connection that delivered is no failure in a row");
        let mut plan = Plan::new(1_000, 1, 100);
        plan.claim(0, 1_000);
        assert_eq!(plan.release(0), None, "nothing left");
    }

    /// Slots take, receive, fail and finish in a pseudo-random order: every
    /// byte is claimed once, and all of them by the end.
    #[test]
    fn every_byte_is_claimed_exactly_once() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut random = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..50 {
            let total = 1 + random(20_000);
            let connections = 1 + random(MAX_CONNECTIONS as u64) as usize;
            let mut plan = Plan::new(total, connections, 1 + random(500));
            let mut claimed = vec![false; total as usize];
            let mut done = vec![false; connections];
            while done.contains(&false) {
                let slot = random(connections as u64) as usize;
                if done[slot] {
                    continue;
                }
                if plan.active[slot].is_none() {
                    done[slot] = plan.take(slot) == Take::Done;
                    continue;
                }
                let (at, take, finished) = plan.claim(slot, 1 + random(700));
                for byte in &mut claimed[at as usize..(at + take) as usize] {
                    assert!(!*byte, "round {round}: byte claimed twice");
                    *byte = true;
                }
                if finished {
                    plan.finish(slot);
                } else if random(8) == 0 {
                    let part = plan.release(slot).expect("not finished");
                    plan.queue.push_back(part);
                }
            }
            assert!(claimed.iter().all(|&byte| byte), "round {round}: every byte");
        }
    }
}

//! `Gateway::download` against a local server that serves a generated file,
//! with byte ranges or without, and misbehaves when told to: a connection
//! that dies or stalls, a file that changes, a length that lies.

use std::collections::HashSet;
use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use hyper::body::{Frame, SizeHint};
use omnid_net::{
    Download, DownloadError, DownloadOptions, Expected, FetchError, Gateway, GatewayConfig, Progress, Route,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

/// 1 MiB: with 64 KiB parts at least, four connections get 256 KiB each.
const LEN: usize = 1 << 20;
const MODIFIED: [&str; 2] = ["Sat, 01 Nov 2026 00:00:00 GMT", "Sun, 02 Nov 2026 00:00:00 GMT"];

/// How the server behaves. All off is a well-behaved server with ranges.
#[derive(Clone, Copy, Default)]
struct Behaviour {
    /// Ignores Range and sends the whole file.
    no_ranges: bool,
    /// The whole file without a Content-Length.
    chunked: bool,
    /// From this request on (the first is 0), another version of the file.
    change_at: Option<usize>,
    /// Answers as if there were no If-Range.
    ignore_if_range: bool,
    /// Only Last-Modified, no ETag.
    no_etag: bool,
    /// Range answers after the first say the file is this much longer.
    lie_total: usize,
    /// Range answers after the first start a byte later than asked.
    shift_start: bool,
    fault: Option<Fault>,
    /// The range that starts here trickles, 1 KiB every 10 ms.
    slow_from: Option<usize>,
    /// Every body goes this many bytes at a time, a millisecond apart.
    pace: Option<usize>,
    disposition: Option<&'static str>,
}

/// Requests `from..from + count` die or stall after `after` bytes of body.
#[derive(Clone, Copy)]
struct Fault {
    from: usize,
    count: usize,
    after: usize,
    kind: FaultKind,
}

#[derive(Clone, Copy, PartialEq)]
enum FaultKind {
    Die,
    Stall,
}

struct Server {
    behaviour: Behaviour,
    /// The file's two versions.
    files: [Bytes; 2],
    requests: AtomicUsize,
    seen: Mutex<Vec<Seen>>,
}

#[derive(Debug, Clone)]
struct Seen {
    n: usize,
    peer: u16,
    range: Option<String>,
    if_range: Option<String>,
}

impl Seen {
    fn start(&self) -> Option<usize> {
        self.range.as_deref()?.strip_prefix("bytes=")?.split('-').next()?.parse().ok()
    }
}

impl Server {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve(behaviour: Behaviour, len: usize) -> (Arc<Server>, SocketAddr) {
    let server = Arc::new(Server {
        behaviour,
        files: [generated(len, 1), generated(len, 2)],
        requests: AtomicUsize::new(0),
        seen: Mutex::default(),
    });
    let redirect = |to: &'static str| move || async move { (StatusCode::FOUND, [(header::LOCATION, to)]) };
    let app = axum::Router::new()
        .route("/file", get(file))
        .route("/moved", get(redirect("/moved-again")))
        .route("/moved-again", get(redirect("file")))
        .route("/loop", get(redirect("/loop")))
        .route(
            "/past-the-end",
            get(|| async { (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, "bytes 0-99/50")], vec![0u8; 100]) }),
        )
        .route(
            "/two-ranges",
            get(|| async {
                let range = (header::CONTENT_RANGE, "bytes 0-9/10");
                (StatusCode::PARTIAL_CONTENT, AppendHeaders([range.clone(), range]), vec![0u8; 10])
            }),
        )
        .with_state(server.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap()
    });
    (server, addr)
}

fn generated(len: usize, seed: u64) -> Bytes {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

async fn file(
    State(server): State<Arc<Server>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let n = server.requests.fetch_add(1, Ordering::SeqCst);
    let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let (range, if_range) = (text(header::RANGE), text(header::IF_RANGE));
    server.seen.lock().unwrap().push(Seen { n, peer: peer.port(), range: range.clone(), if_range: if_range.clone() });

    let b = server.behaviour;
    let version = usize::from(b.change_at.is_some_and(|at| n >= at));
    let data = server.files[version].clone();
    let etag = format!("\"v{version}\"");
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::LAST_MODIFIED, MODIFIED[version]);
    if !b.no_etag {
        response = response.header(header::ETAG, &etag);
    }
    if let Some(disposition) = b.disposition {
        response = response.header(header::CONTENT_DISPOSITION, disposition);
    }
    let current = if_range.is_none_or(|v| b.ignore_if_range || v == etag || v == MODIFIED[version]);
    let range = range.filter(|_| !b.no_ranges && current);
    let fault = b.fault.filter(|f| n >= f.from && n - f.from < f.count);
    if range.is_some() && data.is_empty() {
        return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, "bytes */0")]).into_response();
    }
    let Some((start, last)) = range.and_then(|range| parse_range(&range, data.len())) else {
        let body = Chunks::new(data, b.pace.map(|step| (step, Duration::from_millis(1))), fault, !b.chunked);
        return response.status(StatusCode::OK).body(Body::new(body)).unwrap();
    };
    let later = n > 0;
    let total = data.len() + if later { b.lie_total } else { 0 };
    let shown_start = start + usize::from(later && b.shift_start);
    let pace = match b.slow_from {
        Some(from) if from == start => Some((1 << 10, Duration::from_millis(10))),
        _ => b.pace.map(|step| (step, Duration::from_millis(1))),
    };
    response
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_RANGE, format!("bytes {shown_start}-{last}/{total}"))
        .body(Body::new(Chunks::new(data.slice(start..=last), pace, fault, true)))
        .unwrap()
}

/// `bytes=S-` or `bytes=S-E`, within a file of `len` bytes.
fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let (start, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let last = if last.is_empty() { len - 1 } else { last.parse::<usize>().ok()?.min(len - 1) };
    (start <= last).then_some((start, last))
}

/// A body sent in pieces, `pace` bytes and time apart, until `fault` strikes.
struct Chunks {
    data: Bytes,
    sent: usize,
    pace: Option<(usize, Duration)>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    fault: Option<Fault>,
    /// A pause before dying, so that what was sent goes out first.
    dying: Option<Pin<Box<tokio::time::Sleep>>>,
    /// With a Content-Length.
    exact: bool,
}

impl Chunks {
    fn new(data: Bytes, pace: Option<(usize, Duration)>, fault: Option<Fault>, exact: bool) -> Self {
        Self { data, sent: 0, pace, sleep: None, fault, dying: None, exact }
    }
}

impl hyper::body::Body for Chunks {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        if let Some(fault) = this.fault
            && this.sent >= fault.after
        {
            if fault.kind == FaultKind::Stall {
                // Never woken: the connection stays open with nothing on it.
                return Poll::Pending;
            }
            let dying = this.dying.get_or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_millis(20))));
            ready!(dying.as_mut().poll(cx));
            return Poll::Ready(Some(Err(std::io::Error::other("the server died"))));
        }
        if let Some(sleep) = &mut this.sleep {
            ready!(sleep.as_mut().poll(cx));
            this.sleep = None;
        }
        if this.data.is_empty() {
            return Poll::Ready(None);
        }
        let mut n = this.pace.map_or(64 << 10, |(step, _)| step).min(this.data.len());
        if let Some(fault) = this.fault {
            n = n.min(fault.after - this.sent);
        }
        let piece = this.data.split_to(n);
        this.sent += n;
        if let Some((_, delay)) = this.pace {
            this.sleep = Some(Box::pin(tokio::time::sleep(delay)));
        }
        Poll::Ready(Some(Ok(Frame::data(piece))))
    }

    fn size_hint(&self) -> SizeHint {
        if self.exact { SizeHint::with_exact(self.data.len() as u64) } else { SizeHint::default() }
    }
}

fn gateway() -> Gateway {
    Gateway::new(GatewayConfig {
        tor_dir: None,
        allow_private_addresses: true,
        i2p_proxies: vec![],
        fproxies: vec![],
        ..GatewayConfig::default()
    })
    .unwrap()
}

fn options() -> DownloadOptions {
    DownloadOptions { min_segment: 64 << 10, stall_timeout: Duration::from_millis(500), ..DownloadOptions::default() }
}

/// A file to download into, named after the test and the process (two test
/// runs at once do not share files), in Cargo's temp directory.
fn output(name: &str) -> (PathBuf, File) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("download");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}-{name}", std::process::id()));
    let file = File::options().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
    (path, file)
}

struct Run {
    result: Result<Download, DownloadError>,
    /// What the file holds afterwards.
    bytes: Vec<u8>,
    progress: Progress,
}

async fn download_with(gateway: &Gateway, addr: SocketAddr, path: &str, name: &str, opts: DownloadOptions) -> Run {
    let url = Url::parse(&format!("http://{addr}{path}")).unwrap();
    let (file_path, file) = output(name);
    let progress = Progress::default();
    let result = gateway.download(&url, &file, &opts, &progress, &CancellationToken::new()).await;
    let bytes = std::fs::read(&file_path).unwrap();
    std::fs::remove_file(file_path).unwrap();
    Run { result, bytes, progress }
}

async fn download(addr: SocketAddr, path: &str, name: &str, opts: DownloadOptions) -> Run {
    download_with(&gateway(), addr, path, name, opts).await
}

#[tokio::test]
async fn splits_a_file_among_connections_and_writes_it_whole() {
    let (server, addr) = serve(Behaviour { pace: Some(16 << 10), ..Behaviour::default() }, LEN).await;
    let run = download(addr, "/file", "split", options()).await;
    let download = run.result.unwrap();
    assert!(run.bytes == server.files[0], "byte for byte");
    assert!(download.ranged);
    assert_eq!((download.bytes, download.retries), (LEN as u64, 0));
    assert!((2..=4).contains(&download.connections), "{}", download.connections);
    assert_eq!(download.filename, "file");
    assert_eq!(download.content_type.as_deref(), Some("application/octet-stream"));
    assert_eq!((run.progress.total(), run.progress.done(), run.progress.active()), (LEN as u64, LEN as u64, 0));

    let seen = server.seen();
    assert_eq!(seen[0].range.as_deref(), Some("bytes=0-"));
    assert_eq!(seen[0].if_range, None);
    assert!(seen[1..].iter().all(|s| s.if_range.as_deref() == Some("\"v0\"")), "{seen:?}");
    let connections: HashSet<u16> = seen.iter().map(|s| s.peer).collect();
    assert!(connections.len() >= 4, "a connection for each part: {seen:?}");
    let starts: HashSet<usize> = seen.iter().filter_map(Seen::start).collect();
    assert!([0, LEN / 4, LEN / 2, LEN * 3 / 4].iter().all(|s| starts.contains(s)), "{seen:?}");
}

#[tokio::test]
async fn reads_a_server_without_ranges_in_one_stream() {
    for (case, chunked) in [("single", false), ("single-chunked", true)] {
        let (server, addr) = serve(Behaviour { no_ranges: true, chunked, ..Behaviour::default() }, LEN).await;
        let run = download(addr, "/file", case, options()).await;
        let download = run.result.unwrap();
        assert!(run.bytes == server.files[0], "{case}");
        assert!(!download.ranged && download.connections == 1, "{case}");
        assert_eq!(server.seen().len(), 1, "{case}");
        let total = if chunked { 0 } else { LEN as u64 };
        assert_eq!((run.progress.total(), run.progress.done()), (total, LEN as u64), "{case}");
    }
}

#[tokio::test]
async fn a_small_file_comes_in_one_part() {
    let (server, addr) = serve(Behaviour::default(), 100 << 10).await;
    let run = download(addr, "/file", "small", options()).await;
    let download = run.result.unwrap();
    assert!(run.bytes == server.files[0]);
    assert!(download.ranged && download.connections == 1, "under two minimum parts: not split");
    assert_eq!(server.seen().len(), 1);
}

#[tokio::test]
async fn a_file_that_changes_fails_instead_of_splicing() {
    let changing = Behaviour { change_at: Some(1), ..Behaviour::default() };
    for (case, behaviour) in [
        // If-Range no longer matches: the server sends the new file whole.
        ("if-range", changing),
        // A server that ignores If-Range still says which version it sent.
        ("etag", Behaviour { ignore_if_range: true, ..changing }),
        // Only a date to go by.
        ("date", Behaviour { no_etag: true, ..changing }),
        ("date-ignored", Behaviour { no_etag: true, ignore_if_range: true, ..changing }),
    ] {
        let (server, addr) = serve(behaviour, LEN).await;
        let run = download(addr, "/file", &format!("changed-{case}"), options()).await;
        assert!(matches!(run.result, Err(DownloadError::Changed)), "{case}: {:?}", run.result);
        let [old, new] = &server.files;
        let spliced = run
            .bytes
            .iter()
            .zip(new.iter().zip(old.iter()))
            .filter(|(got, (new, old))| got == new && new != old && **got != 0);
        assert_eq!(spliced.count(), 0, "{case}: nothing of the new version was written");
        if case.starts_with("date") {
            let seen = server.seen();
            assert!(seen[1..].iter().all(|s| s.if_range.as_deref() == Some(MODIFIED[0])), "{case}: {seen:?}");
        }
    }
}

#[tokio::test]
async fn a_connection_that_dies_or_stalls_is_retried_and_the_file_is_whole() {
    for (case, kind) in [("died", FaultKind::Die), ("stalled", FaultKind::Stall)] {
        let fault = Fault { from: 1, count: 1, after: 100 << 10, kind };
        let behaviour = Behaviour { fault: Some(fault), pace: Some(16 << 10), ..Behaviour::default() };
        let (server, addr) = serve(behaviour, LEN).await;
        let opts = DownloadOptions { stall_timeout: Duration::from_millis(300), ..options() };
        let started = Instant::now();
        // Bounded: without a stall timeout, a stalled connection never ends.
        let run = tokio::time::timeout(Duration::from_secs(10), download(addr, "/file", case, opts))
            .await
            .unwrap_or_else(|_| panic!("{case}: still running after 10 s"));
        let download = run.result.unwrap_or_else(|e| panic!("{case}: {e}"));
        assert!(run.bytes == server.files[0], "{case}: byte for byte");
        assert_eq!(download.retries, 1, "{case}");
        let seen = server.seen();
        let failed = seen.iter().find(|s| s.n == 1).and_then(Seen::start).unwrap();
        assert!(
            seen.iter().any(|s| s.start() == Some(failed + (100 << 10))),
            "{case}: resumed where it stopped: {seen:?}"
        );
        if kind == FaultKind::Stall {
            assert!(started.elapsed() >= Duration::from_millis(300), "{case}: waited for the stall timeout");
        }
    }
}

#[tokio::test]
async fn a_part_that_keeps_failing_fails_the_download() {
    let fault = Fault { from: 0, count: usize::MAX, after: 1 << 10, kind: FaultKind::Die };
    let (server, addr) = serve(Behaviour { fault: Some(fault), ..Behaviour::default() }, LEN).await;
    let opts = DownloadOptions { connections: 1, max_retries: 2, ..options() };
    let run = download(addr, "/file", "keeps-failing", opts).await;
    let error = run.result.unwrap_err();
    assert!(matches!(error, DownloadError::Fetch(FetchError::Transport { .. })), "{error}");
    assert_eq!(run.progress.retries(), 2);
    assert_eq!(server.seen().len(), 3, "the first try and two retries");
    assert!(run.bytes[..3 << 10] == server.files[0][..3 << 10], "each delivered 1 KiB, resumed after the last");
}

#[tokio::test]
async fn a_length_or_range_that_does_not_hold_fails() {
    let (_, addr) = serve(Behaviour { lie_total: 1, ..Behaviour::default() }, LEN).await;
    let run = download(addr, "/file", "lying-total", options()).await;
    assert!(matches!(run.result, Err(DownloadError::Changed)), "{:?}", run.result);

    let (_, addr) = serve(Behaviour { shift_start: true, ..Behaviour::default() }, LEN).await;
    let run = download(addr, "/file", "lying-start", options()).await;
    assert!(matches!(run.result, Err(DownloadError::Protocol(_))), "{:?}", run.result);

    for path in ["/past-the-end", "/two-ranges"] {
        let run = download(addr, path, &format!("malformed{}", path.replace('/', "-")), options()).await;
        assert!(matches!(run.result, Err(DownloadError::Protocol(_))), "{path}: {:?}", run.result);
        assert!(run.bytes.is_empty(), "{path}");
    }
}

#[tokio::test]
async fn refuses_files_over_the_limit() {
    let max_bytes = LEN as u64 - 1;
    for (case, behaviour) in [
        ("too-large-ranged", Behaviour::default()),
        ("too-large-single", Behaviour { no_ranges: true, ..Behaviour::default() }),
    ] {
        let (server, addr) = serve(behaviour, LEN).await;
        let run = download(addr, "/file", case, DownloadOptions { max_bytes, ..options() }).await;
        assert!(matches!(run.result, Err(DownloadError::TooLarge { limit }) if limit == max_bytes), "{case}");
        assert!(run.bytes.is_empty(), "{case}: refused before anything was written");
        assert_eq!(server.seen().len(), 1, "{case}");
    }
    // Without a length, the stream is stopped at the limit.
    let (server, addr) = serve(Behaviour { no_ranges: true, chunked: true, ..Behaviour::default() }, LEN).await;
    let max_bytes = 300 << 10;
    let run = download(addr, "/file", "too-large-chunked", DownloadOptions { max_bytes, ..options() }).await;
    assert!(matches!(run.result, Err(DownloadError::TooLarge { .. })), "{:?}", run.result);
    assert!(run.bytes.len() as u64 <= max_bytes && server.files[0].starts_with(&run.bytes));
}

#[tokio::test]
async fn follows_redirects_and_gives_up_on_loops() {
    let (server, addr) = serve(Behaviour::default(), LEN).await;
    let run = download(addr, "/moved", "redirected", options()).await;
    assert_eq!(run.result.unwrap().url.path(), "/file");
    assert!(run.bytes == server.files[0]);

    let run = download(addr, "/loop", "looping", options()).await;
    assert!(matches!(run.result, Err(DownloadError::TooManyRedirects)), "{:?}", run.result);
}

#[tokio::test]
async fn a_file_other_than_the_one_expected_is_a_mismatch() {
    let (_, addr) = serve(Behaviour::default(), LEN).await;
    let expecting = |expect| DownloadOptions { expect: Some(expect), ..options() };
    for (case, expect) in [
        ("length", Expected { length: Some(LEN as u64 + 1), ..Expected::default() }),
        ("etag", Expected { etag: Some("\"other\"".into()), ..Expected::default() }),
        ("date", Expected { last_modified: Some(MODIFIED[1].into()), ..Expected::default() }),
    ] {
        let run = download(addr, "/file", &format!("mismatch-{case}"), expecting(expect)).await;
        assert!(matches!(run.result, Err(DownloadError::Mismatch(_))), "{case}: {:?}", run.result);
        assert!(run.bytes.is_empty(), "{case}: nothing written");
    }
    // A weak tag for the same version matches: the caller may have seen the
    // file compressed.
    let same =
        Expected { length: Some(LEN as u64), etag: Some("W/\"v0\"".into()), last_modified: Some(MODIFIED[0].into()) };
    download(addr, "/file", "expected", expecting(same)).await.result.unwrap();

    let (_, addr) = serve(Behaviour { no_ranges: true, chunked: true, ..Behaviour::default() }, LEN).await;
    let short = Expected { length: Some(LEN as u64 - 1), ..Expected::default() };
    let run = download(addr, "/file", "mismatch-stream", expecting(short)).await;
    assert!(matches!(run.result, Err(DownloadError::Mismatch(_))), "a stream's length is checked at its end");
}

#[tokio::test]
async fn a_slow_connection_does_not_hold_the_download() {
    // Alone, the part from LEN / 4 would take 2.6 s at 1 KiB every 10 ms.
    let (server, addr) = serve(Behaviour { slow_from: Some(LEN / 4), ..Behaviour::default() }, LEN).await;
    let started = Instant::now();
    let run = download(addr, "/file", "stealing", DownloadOptions { min_segment: 16 << 10, ..options() }).await;
    run.result.unwrap();
    assert!(run.bytes == server.files[0]);
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(1_500), "{elapsed:?}");
    let seen = server.seen();
    assert!(
        seen.iter().filter_map(Seen::start).any(|s| s > LEN / 4 && s < LEN / 2),
        "the slow part was split: {seen:?}"
    );
}

#[tokio::test]
async fn cancelling_stops_the_download() {
    let (_, addr) = serve(Behaviour { pace: Some(1 << 10), ..Behaviour::default() }, LEN).await;
    let url = Url::parse(&format!("http://{addr}/file")).unwrap();
    let (path, file) = output("cancelled");
    let (progress, cancel) = (Progress::default(), CancellationToken::new());
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
    });
    let result = gateway().download(&url, &file, &options(), &progress, &cancel).await;
    assert!(matches!(result, Err(DownloadError::Cancelled)), "{result:?}");
    assert!(progress.done() < LEN as u64);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn names_the_file_as_the_server_does() {
    let disposition = "attachment; filename=\"fallback.pdf\"; filename*=UTF-8''..%2Fr%C3%A9sum%C3%A9.pdf";
    let (_, addr) = serve(Behaviour { disposition: Some(disposition), ..Behaviour::default() }, 1_000).await;
    let run = download(addr, "/file", "named", options()).await;
    assert_eq!(run.result.unwrap().filename, "résumé.pdf");
}

#[tokio::test]
async fn an_empty_file_is_empty() {
    let (_, addr) = serve(Behaviour::default(), 0).await;
    let run = download(addr, "/file", "empty", options()).await;
    let download = run.result.unwrap();
    assert_eq!(download.bytes, 0);
    assert!(run.bytes.is_empty());
}

#[tokio::test]
async fn huge_headers_are_an_error_not_a_hang() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = stream.read(&mut [0u8; 4096]).await;
                let _ = stream.write_all(b"HTTP/1.1 206 Partial Content\r\n").await;
                let line = format!("X-Junk: {}\r\n", "j".repeat(1_000));
                for _ in 0..4_000 {
                    if stream.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let run = tokio::time::timeout(Duration::from_secs(20), download(addr, "/file", "huge-headers", options()))
        .await
        .expect("no hang");
    assert!(matches!(run.result, Err(DownloadError::Fetch(FetchError::Transport { .. }))), "{:?}", run.result);
}

#[tokio::test]
async fn only_http_to_public_addresses_and_never_direct_for_the_browser() {
    let (server, addr) = serve(Behaviour::default(), 1_000).await;
    let guarded = Gateway::new(GatewayConfig {
        tor_dir: None,
        i2p_proxies: vec![],
        fproxies: vec![],
        ..GatewayConfig::default()
    })
    .unwrap();
    let run = download_with(&guarded, addr, "/file", "private", options()).await;
    assert!(matches!(run.result, Err(DownloadError::Fetch(FetchError::PrivateAddress(_)))), "{:?}", run.result);

    // The browser's downloads go through Tor, which is off: not directly.
    let browser = DownloadOptions { route: Route::Browser { site: "127.0.0.1".into() }, ..options() };
    let run = download(addr, "/file", "browser", browser).await;
    assert!(matches!(run.result, Err(DownloadError::Fetch(FetchError::Unavailable { .. }))), "{:?}", run.result);
    assert!(server.seen().is_empty(), "nothing reached the server");

    let (path, file) = output("ftp");
    let url = Url::parse("ftp://example.com/file").unwrap();
    let result = gateway().download(&url, &file, &options(), &Progress::default(), &CancellationToken::new()).await;
    assert!(matches!(result, Err(DownloadError::Unsupported(_))), "{result:?}");
    std::fs::remove_file(path).unwrap();
}

//! The local HTTP API and the search page that uses it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlControl, DISCOVERED_TRUST, Job, Safety, Verdict};
use omnid_dht::Dht;
use omnid_index::{SearchIndex, SearchQuery, SearchResults};
use omnid_net::{Gateway, RouterState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

/// Crawled titles and descriptions are attacker-controlled; the page renders
/// them as text, and this policy is the second line of defence.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; \
                   base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const TOKEN_COOKIE: &str = "omnid_token";

#[derive(Clone)]
pub struct AppState {
    pub gateway: Arc<Gateway>,
    pub index: Arc<SearchIndex>,
    pub control: Arc<CrawlControl>,
    pub jobs: mpsc::Sender<Job>,
    pub safety: Arc<Safety>,
    pub dht: Option<Dht>,
}

/// `token`, when set, is required on every request as a bearer token or the
/// `omnid_token` cookie. Loopback is reachable by every app on a phone and
/// every user on a shared machine; the token keeps them out.
pub fn router(state: AppState, listen: SocketAddr, token: Option<String>) -> Router {
    let mut router = Router::new()
        .route("/", get(|| asset("text/html; charset=utf-8", include_bytes!("ui/index.html"))))
        .route("/app.js", get(|| asset("text/javascript; charset=utf-8", include_bytes!("ui/app.js"))))
        .route("/app.css", get(|| asset("text/css; charset=utf-8", include_bytes!("ui/app.css"))))
        .route("/icon.png", get(|| asset("image/png", include_bytes!("ui/icon.png"))))
        .route("/api/search", get(search))
        .route("/api/submit", post(submit))
        .route("/api/status", get(status))
        .route("/api/crawl", post(crawl))
        .with_state(state);
    if let Some(token) = token {
        router = router.layer(middleware::from_fn_with_state(Arc::<str>::from(token), require_token));
    }
    if listen.ip().is_loopback() {
        router = router.layer(middleware::from_fn_with_state(listen.port(), loopback_host_only));
    }
    router
}

/// Any web page can make the browser send requests to 127.0.0.1. Requiring a
/// loopback `Host` header stops DNS rebinding, whose requests carry the
/// attacker's hostname. Cross-site `POST`s are stopped separately: a JSON body
/// needs a CORS preflight, which this server never grants.
async fn loopback_host_only(State(port): State<u16>, request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    let host = host.strip_suffix(&format!(":{port}")).unwrap_or(if port == 80 { host } else { "" });
    if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        next.run(request).await
    } else {
        ApiError(StatusCode::FORBIDDEN, "requests must be addressed to localhost".into()).into_response()
    }
}

async fn require_token(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let bearer =
        headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer "));
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .find_map(|c| c.trim().strip_prefix(TOKEN_COOKIE)?.strip_prefix('='));
    if bearer.or(cookie).is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes())) {
        next.run(request).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "missing or wrong API token".into()).into_response()
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

async fn asset(content_type: &'static str, body: &'static [u8]) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    /// Comma-separated network names.
    #[serde(default)]
    network: String,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
    /// Comma-separated languages to rank first; `Accept-Language` otherwise.
    lang: Option<String>,
}

#[derive(Serialize)]
struct SearchResponse {
    took_ms: u64,
    #[serde(flatten)]
    results: SearchResults,
}

async fn search(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let languages = match &params.lang {
        Some(lang) => omnid_core::lang::preferred(lang),
        None => headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|h| h.to_str().ok())
            .map(omnid_core::lang::preferred)
            .unwrap_or_default(),
    };
    let networks = params
        .network
        .split(',')
        .filter(|n| !n.trim().is_empty())
        .map(str::parse)
        .collect::<Result<Vec<Network>, _>>()
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let query =
        SearchQuery { text: params.q, networks, offset: params.offset, limit: params.limit.unwrap_or(20), languages };
    let started = Instant::now();
    let index = state.index.clone();
    let results = tokio::task::spawn_blocking(move || index.search(&query))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(SearchResponse { took_ms: started.elapsed().as_millis() as u64, results }))
}

#[derive(Deserialize)]
struct Submission {
    url: String,
}

/// Queues an address for crawling now, even if it is already known.
async fn submit(State(state): State<AppState>, Json(submission): Json<Submission>) -> Result<Response, ApiError> {
    let source = Source::parse(&submission.url).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    if state.safety.check_source(&source) != Verdict::Allow {
        return Err(ApiError(StatusCode::FORBIDDEN, "that site is on the denylist".into()));
    }
    let accepted = source.to_string();
    let network = source.network();
    state
        .jobs
        .try_send(Job { source, depth: 0, trust: DISCOVERED_TRUST })
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "the crawl queue is full; try again shortly".into()))?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": accepted, "network": network }))).into_response())
}

#[derive(Deserialize)]
struct CrawlState {
    paused: bool,
}

async fn crawl(State(state): State<AppState>, Json(desired): Json<CrawlState>) -> Json<serde_json::Value> {
    state.control.set_paused(desired.paused);
    Json(json!({ "crawl": state.control.stats() }))
}

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut routers = state.gateway.status().await;
    let dht = state.dht.as_ref().map(Dht::status);
    if let (Some(dht), Some(magnet)) = (&dht, routers.iter_mut().find(|r| r.network == Network::Magnet)) {
        magnet.state = if dht.ready { RouterState::Ready } else { RouterState::Starting };
        magnet.detail = format!("DHT, {} nodes", dht.nodes);
    }
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "routers": routers,
        "dht": dht,
        "index": { "documents": state.index.num_docs() },
        "crawl": state.control.stats(),
        "pause_reason": state.control.pause_reason(),
    }))
}

struct ApiError(StatusCode, String);

impl ApiError {
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!("api: {error}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens_by_value() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-tokem"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}

//! The local HTTP API and the search page that uses it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use omnid_core::{Network, Source};
use omnid_crawl::{CrawlStats, DISCOVERED_TRUST, Job, Safety, Verdict};
use omnid_index::{SearchHit, SearchIndex, SearchQuery};
use omnid_net::Gateway;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

/// Crawled titles and descriptions are attacker-controlled; the page renders
/// them as text, and this policy is the second line of defence.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

#[derive(Clone)]
pub struct AppState {
    pub gateway: Arc<Gateway>,
    pub index: Arc<SearchIndex>,
    pub stats: Arc<CrawlStats>,
    pub jobs: mpsc::Sender<Job>,
    pub safety: Arc<Safety>,
}

pub fn router(state: AppState, listen: SocketAddr) -> Router {
    let router = Router::new()
        .route("/", get(|| asset("text/html; charset=utf-8", include_str!("ui/index.html"))))
        .route("/app.js", get(|| asset("text/javascript; charset=utf-8", include_str!("ui/app.js"))))
        .route("/app.css", get(|| asset("text/css; charset=utf-8", include_str!("ui/app.css"))))
        .route("/api/search", get(search))
        .route("/api/submit", post(submit))
        .route("/api/status", get(status))
        .with_state(state);
    if listen.ip().is_loopback() {
        router.layer(middleware::from_fn_with_state(listen.port(), loopback_host_only))
    } else {
        router
    }
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

async fn asset(content_type: &'static str, body: &'static str) -> Response {
    let mut response = Html(body).into_response();
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
    limit: Option<usize>,
}

#[derive(Serialize)]
struct SearchResponse {
    took_ms: u64,
    hits: Vec<SearchHit>,
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let networks = params
        .network
        .split(',')
        .filter(|n| !n.trim().is_empty())
        .map(str::parse)
        .collect::<Result<Vec<Network>, _>>()
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let query = SearchQuery { text: params.q, networks, limit: params.limit.unwrap_or(20) };
    let started = Instant::now();
    let index = state.index.clone();
    let hits = tokio::task::spawn_blocking(move || index.search(&query))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    Ok(Json(SearchResponse { took_ms: started.elapsed().as_millis() as u64, hits }))
}

#[derive(Deserialize)]
struct Submission {
    url: String,
}

/// Queues an address for crawling. Already-crawled addresses are accepted
/// and ignored.
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

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "routers": state.gateway.status().await,
        "index": { "documents": state.index.num_docs() },
        "crawl": state.stats.snapshot(),
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

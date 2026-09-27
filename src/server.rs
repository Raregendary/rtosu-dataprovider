use crate::overlays::{self, OverlayStore};
use crate::v2::TosuV2Packet;
use anyhow::{Context, Result};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

/// One poll's packet plus the JSON every consumer receives.
///
/// The payload is ~99 % strain graph, so encoding it once per tick and sharing
/// the result removes a per-client `serde_json::to_string` of ~0.6-1.2 MB.
/// `Bytes` is a refcounted buffer, so a consumer's cost is a refcount bump.
#[derive(Clone, Debug)]
pub struct PublishedPacket {
    pub packet: Arc<TosuV2Packet>,
    pub json: Bytes,
}

impl PublishedPacket {
    /// Serialize the packet once. `None` means the packet could not be encoded,
    /// which is logged instead of being pushed to clients as an empty message.
    pub fn new(packet: TosuV2Packet) -> Option<Self> {
        crate::instr_scope!(JsonEncode);
        match serde_json::to_vec(&packet) {
            Ok(json) => Some(Self {
                packet: Arc::new(packet),
                json: Bytes::from(json),
            }),
            Err(err) => {
                tracing::error!("failed to serialize packet: {err:#}");
                None
            }
        }
    }

    pub fn default_packet() -> Self {
        let packet = TosuV2Packet::default();
        let json = serde_json::to_vec(&packet).unwrap_or_else(|_| b"{}".to_vec());
        Self {
            packet: Arc::new(packet),
            json: Bytes::from(json),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub packet_rx: watch::Receiver<PublishedPacket>,
    /// User-supplied browser overlays. `None` disables the overlay routes.
    pub overlays: Option<Arc<OverlayStore>>,
}

impl AppState {
    pub fn new(packet_rx: watch::Receiver<PublishedPacket>) -> Self {
        Self {
            packet_rx,
            overlays: None,
        }
    }

    /// Serve overlays from `root`, re-scanning it when its contents change.
    pub fn with_overlays(
        packet_rx: watch::Receiver<PublishedPacket>,
        root: std::path::PathBuf,
    ) -> Self {
        Self {
            packet_rx,
            overlays: Some(Arc::new(OverlayStore::new(root))),
        }
    }
}

pub fn create_router(
    state: AppState,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
) -> Router {
    let mut router = Router::new();

    if enable_http {
        router = router
            .route("/json/v2", get(handle_json_v2))
            .route("/json/v2/precise", get(handle_json_v2))
            .route("/json", get(handle_json_v2))
            .route("/health", get(handle_health))
            // tosu file endpoints that overlays use to display the current
            // beatmap background, so drop-in overlays render unchanged.
            .route("/files/beatmap/background", get(handle_beatmap_background))
            .route("/backgroundImage", get(handle_beatmap_background))
            // Overlay pages trigger this automatically; answer it so the
            // browser console stays free of spurious 404s.
            .route("/favicon.ico", get(handle_favicon));
    }

    if enable_ws {
        router = router
            .route("/websocket/v2", get(handle_ws_upgrade))
            .route("/websocket/v2/precise", get(handle_ws_upgrade));
    }

    if state.overlays.is_some() {
        // Static segments win over `{slug}` in the router, so the generated shim
        // cannot be shadowed by an overlay folder of the same name.
        router = router
            .route(
                &format!("{}/__rtosu/overlay-shim.js", overlays::OVERLAYS_BASE),
                get(handle_overlay_shim),
            )
            .route(overlays::OVERLAYS_BASE, get(handle_overlays_index))
            .route(
                &format!("{}/", overlays::OVERLAYS_BASE),
                get(handle_overlays_index),
            )
            .route(
                &format!("{}/{{slug}}", overlays::OVERLAYS_BASE),
                get(handle_overlay_redirect),
            )
            .route(
                &format!("{}/{{slug}}/", overlays::OVERLAYS_BASE),
                get(handle_overlay_entry),
            )
            .route(
                &format!("{}/{{slug}}/{{*path}}", overlays::OVERLAYS_BASE),
                get(handle_overlay_asset),
            );
    }

    if cors_allow_all {
        router = router.layer(CorsLayer::permissive());
    }

    router.with_state(state)
}

fn json_response(json: Bytes) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(json),
    )
        .into_response()
}

async fn handle_json_v2(State(state): State<AppState>) -> Response {
    let published = state.packet_rx.borrow().clone();
    json_response(published.json)
}

async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    let published = state.packet_rx.borrow();
    let packet = &published.packet;
    Json(serde_json::json!({
        "status": "ok",
        "client": packet.client,
        "state": packet.state.name,
        "tourneyClients": packet.tourney.clients.len(),
    }))
}

fn text_response(body: impl Into<Body>, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body.into()).into_response()
}

/// Read a single value out of a raw query string.
fn query_value(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if overlays::percent_decode(&name.replace('+', " ")) == key {
            return Some(overlays::percent_decode(&value.replace('+', " ")));
        }
    }
    None
}

/// Serve the current beatmap background so drop-in tosu overlays can display it.
///
/// tosu answers the same request on `/backgroundImage?mapset=<id>`, so a
/// mismatched mapset is rejected rather than returning the wrong image.
async fn handle_beatmap_background(State(state): State<AppState>, raw_query: RawQuery) -> Response {
    // Copy out of the watch guard before awaiting: the guard is not `Send` and
    // handler futures must be.
    let (background, songs, relative, folder, loaded_set) = {
        let published = state.packet_rx.borrow();
        let packet = &published.packet;
        (
            packet.files.background.clone(),
            packet.folders.songs.clone(),
            packet.direct_path.beatmap_background.clone(),
            packet.direct_path.beatmap_folder.clone(),
            packet.beatmap.set,
        )
    };

    if let Some(requested) = query_value(raw_query.0.as_deref(), "mapset") {
        let matches = requested
            .trim()
            .parse::<i32>()
            .map(|id| id == loaded_set)
            .unwrap_or(false);
        if !matches {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "requested mapset is not the loaded beatmap",
            )
                .into_response();
        }
    }

    // tosu reports the background as a path relative to the songs folder, so
    // try that first and fall back to treating the values as absolute paths.
    let songs_folder = std::path::Path::new(songs.trim());
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if !songs.trim().is_empty() && !relative.trim().is_empty() {
        candidates.push(songs_folder.join(relative.trim()));
    }
    if !background.trim().is_empty() {
        candidates.push(std::path::PathBuf::from(background.trim()));
    }

    let path = candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .or_else(|| find_beatmap_background(songs_folder, &folder, loaded_set));

    let Some(path) = path else {
        tracing::debug!("beatmap background is not readable on disk");
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "beatmap background is not available on disk",
        )
            .into_response();
    };

    let content_type = overlays::content_type_for(&path);
    match overlays::read_file(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                // The background changes with every map, so it must not be cached.
                (header::CACHE_CONTROL, "no-store"),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(err) => {
            tracing::debug!("serving beatmap background failed: {err:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "beatmap background is not available",
            )
                .into_response()
        }
    }
}

/// The overlay store, or `None` when overlays are disabled.
fn overlay_store(state: &AppState) -> Option<Arc<OverlayStore>> {
    state.overlays.clone()
}

async fn handle_overlays_index(State(state): State<AppState>) -> Response {
    let Some(store) = overlay_store(&state) else {
        return not_found("browser overlays are disabled");
    };

    let found = store.list().await;
    let html = overlays::dashboard_html(&found, &store.root);
    text_response(html, "text/html; charset=utf-8")
}

async fn handle_overlay_shim() -> Response {
    let body = Body::from(overlays::OVERLAY_SHIM_JS);
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            // The shim must never be stale, or dropped-in overlays break in
            // ways that are hard to diagnose from an OBS source.
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn handle_overlay_redirect(Path(slug): Path<String>) -> Response {
    Redirect::permanent(&format!("{}/{}/", overlays::OVERLAYS_BASE, slug)).into_response()
}

async fn handle_overlay_entry(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_overlay_file(state, slug, "index.html".to_string(), headers).await
}

async fn handle_overlay_asset(
    State(state): State<AppState>,
    Path((slug, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    handle_overlay_file(state, slug, path, headers).await
}

/// Serve one file from an overlay folder, injecting the compatibility shim into
/// HTML so the overlay finds the data provider regardless of how it was written.
async fn handle_overlay_file(
    state: AppState,
    slug: String,
    path: String,
    headers: HeaderMap,
) -> Response {
    let Some(store) = overlay_store(&state) else {
        return not_found("browser overlays are disabled");
    };

    let Some(overlay) = store.find(&slug).await else {
        return not_found("overlay not found");
    };

    // An empty path means the folder root, which is the entry document.
    let requested = if path.is_empty() || path == "/" {
        overlay.index.clone()
    } else {
        match overlays::resolve_within(&overlay.dir, &path) {
            Some(resolved) => resolved,
            None => return not_found("overlay file not found"),
        }
    };

    // Overlays are edited in place while a tournament is running, so serve them
    // revalidatable rather than cacheable. Without a validator a browser
    // heuristically caches these and silently runs a stale copy.
    let etag = file_etag(&requested);

    if let Some(current) = headers.get(header::IF_NONE_MATCH)
        && matches_etag(current.to_str().unwrap_or_default(), &etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::CACHE_CONTROL, "no-cache"),
                (header::ETAG, etag.as_str()),
            ],
        )
            .into_response();
    }

    match overlays::read_file(&requested).await {
        Ok(bytes) => {
            let content_type = overlays::content_type_for(&requested);
            if is_html(&requested) {
                let html = String::from_utf8_lossy(&bytes);
                let shim = match headers.get(header::HOST) {
                    Some(host) => overlays::shim_url_for_host(host.to_str().unwrap_or_default()),
                    None => overlays::shim_url(),
                };
                let body = overlays::inject_shim(&html, &shim);
                return (
                    [
                        (header::CONTENT_TYPE, content_type),
                        (header::CACHE_CONTROL, "no-cache"),
                        (header::ETAG, etag.as_str()),
                        // The injected shim URL embeds the request host.
                        (header::VARY, "Host"),
                    ],
                    Body::from(body),
                )
                    .into_response();
            }
            (
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::CACHE_CONTROL, "no-cache"),
                    (header::ETAG, etag.as_str()),
                ],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(err) => {
            tracing::debug!("overlay file {} unreadable: {err:#}", requested.display());
            not_found("overlay file not found")
        }
    }
}

/// Whether an `If-None-Match` header matches the current ETag.
///
/// Handles a comma separated list and the `*` wildcard, and treats a weak
/// validator as equal to its strong form, which is what browsers send.
fn matches_etag(header_value: &str, etag: &str) -> bool {
    fn bare(value: &str) -> &str {
        value.trim().trim_start_matches("W/").trim_matches('"')
    }

    if header_value.trim() == "*" {
        return true;
    }

    let current = bare(etag);

    header_value
        .split(',')
        .any(|candidate| bare(candidate) == current)
}

/// Weak validator built from the file's size and modification time.
///
/// Good enough to notice an overlay being edited: any write changes the
/// timestamp, so a reloaded page always picks up new content.
fn file_etag(path: &std::path::Path) -> String {
    let stamp = std::fs::metadata(path)
        .map(|meta| {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|delta| delta.as_nanos())
                .unwrap_or_default();
            format!("{:x}-{:x}", meta.len(), modified)
        })
        .unwrap_or_else(|_| "0".to_string());
    format!("W/\"{stamp}\"")
}

async fn handle_favicon() -> Response {
    // No icon to serve: 204 stops the browser from retrying and logging a 404.
    StatusCode::NO_CONTENT.into_response()
}

/// Locate a beatmap background image inside its songs folder.
///
/// `files.background` comes from a memory read that does not always yield a
/// filename, and `direct_path.beatmap_background` can name the beatmap folder
/// rather than an image. A beatmap folder normally holds exactly one image, so
/// fall back to osu!'s conventional names and then to a single image in the
/// folder. Only the immediate folder is inspected, and only when it sits inside
/// the songs folder.
fn find_beatmap_background(
    songs: &std::path::Path,
    beatmap_folder: &str,
    mapset: i32,
) -> Option<std::path::PathBuf> {
    let name = beatmap_folder.trim();
    if name.is_empty() || name.contains(['/', '\\', ':']) {
        return None;
    }

    let dir = songs.join(name);
    if !dir.is_dir() {
        return None;
    }

    for conventional in ["background.jpg", "background.jpeg", "background.png"] {
        let candidate = dir.join(conventional);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // A ranked mapset's background is usually named `<set>-<id>.jpg`, so
    // prefer that over any other image sitting in the folder.
    let prefixed = format!("{mapset}-");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut fallback: Option<std::path::PathBuf> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_image = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                let ext = ext.to_ascii_lowercase();
                ext == "jpg" || ext == "jpeg" || ext == "png"
            })
            .unwrap_or(false);
        if !is_image {
            continue;
        }

        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&prefixed))
        {
            return Some(path);
        }
        fallback.get_or_insert(path);
    }

    fallback
}

fn is_html(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("html") | Some("htm")
    )
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::debug!("Incoming WebSocket upgrade request");
    ws.on_upgrade(move |socket| handle_ws_stream(socket, state.packet_rx))
}

fn ws_text(json: Bytes) -> Message {
    Message::Text(
        Utf8Bytes::try_from(json).expect("packet json is produced by serde_json as utf-8"),
    )
}

async fn handle_ws_stream(mut socket: WebSocket, mut packet_rx: watch::Receiver<PublishedPacket>) {
    tracing::debug!("WebSocket client connected");

    // Send immediate initial state
    let initial_json = packet_rx.borrow_and_update().json.clone();
    if !initial_json.is_empty() && socket.send(ws_text(initial_json)).await.is_err() {
        tracing::debug!("WebSocket client disconnected during initial handshake");
        return;
    }

    // Stream updates on each tick. The payload was encoded once by the poll
    // loop, so this is a refcount bump and a write, not a re-serialization.
    while packet_rx.changed().await.is_ok() {
        let json_str = packet_rx.borrow_and_update().json.clone();
        if socket.send(ws_text(json_str)).await.is_err() {
            break;
        }
    }

    tracing::debug!("WebSocket client disconnected");
}

/// Start the tosu-compatible HTTP and WebSocket server.
/// If both `enable_http` and `enable_ws` are false, the function immediately
/// returns `Ok(())` without binding any TCP port (zero-port bypass).
/// `overlays_dir` enables user-supplied browser overlays when set.
/// Also configures graceful shutdown listening for termination signals.
#[allow(clippy::too_many_arguments)]
pub async fn start_server(
    host: &str,
    port: u16,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
    overlays_dir: Option<std::path::PathBuf>,
    packet_rx: watch::Receiver<PublishedPacket>,
) -> Result<()> {
    if !enable_http && !enable_ws {
        tracing::info!(
            "Zero-port bypass active: both HTTP and WebSocket are disabled; TCP socket will not be bound."
        );
        return Ok(());
    }

    let listener = bind_listener(host, port).await?;
    serve_with_listener(
        listener,
        enable_http,
        enable_ws,
        cors_allow_all,
        overlays_dir,
        packet_rx,
    )
    .await
}

/// Bind a TCP listener to the configured host and port.
pub async fn bind_listener(host: &str, port: u16) -> Result<tokio::net::TcpListener> {
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .with_context(|| format!("invalid host:port '{}:{}'", host, port))?;

    tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding TCP listener to {}", addr))
}

/// Run Axum HTTP and WebSocket serving loop on an already bound TCP listener.
pub async fn serve_with_listener(
    listener: tokio::net::TcpListener,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
    overlays_dir: Option<std::path::PathBuf>,
    packet_rx: watch::Receiver<PublishedPacket>,
) -> Result<()> {
    let state = match overlays_dir {
        Some(root) => AppState::with_overlays(packet_rx, root),
        None => AppState::new(packet_rx),
    };
    let app = create_router(state, enable_http, enable_ws, cors_allow_all);

    if let Ok(addr) = listener.local_addr() {
        tracing::info!("Listening on TCP socket {}", addr);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Server received shutdown signal, closing active listeners");
        })
        .await
        .context("running axum server")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_http_json_v2_endpoint() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.play.score = 55555;

        let (tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let rx_holder = rx.clone();
        let state = AppState::new(rx);
        let app = create_router(state, true, true, true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/json/v2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(parsed["client"], "stable");
        assert_eq!(parsed["play"]["score"], 55555);

        // Verify state update propagation to active receivers
        let mut updated = TosuV2Packet::default();
        updated.client = "tournament".to_string();
        tx.send(PublishedPacket::new(updated).expect("serialize"))
            .unwrap();
        assert_eq!(rx_holder.borrow().packet.client, "tournament");
    }

    #[tokio::test]
    async fn test_router_conditional_routes_http_only() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let state = AppState::new(rx);
        let app = create_router(state, true, false, true);

        // HTTP endpoint should be 200 OK
        let http_res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/json/v2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(http_res.status(), StatusCode::OK);

        // WS endpoint should be 404 NOT_FOUND
        let ws_res = app
            .oneshot(
                Request::builder()
                    .uri("/websocket/v2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ws_res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_router_conditional_routes_ws_only() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let state = AppState::new(rx);
        let app = create_router(state, false, true, true);

        // HTTP endpoint should be 404 NOT_FOUND
        let http_res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/json/v2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(http_res.status(), StatusCode::NOT_FOUND);

        // WS endpoint should be accessible (status is NOT 404; without WS headers it returns 400 Bad Request)
        let ws_res = app
            .oneshot(
                Request::builder()
                    .uri("/websocket/v2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(ws_res.status(), StatusCode::NOT_FOUND);
        assert_eq!(ws_res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_router_cors_toggle() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();

        // 1. With CORS allowed
        let (_tx, rx1) = watch::channel(PublishedPacket::new(sample.clone()).expect("serialize"));
        let app_cors_enabled = create_router(AppState::new(rx1), true, false, true);
        let res1 = app_cors_enabled
            .oneshot(
                Request::builder()
                    .uri("/json/v2")
                    .header("Origin", "http://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res1.headers().get("access-control-allow-origin").unwrap(),
            "*"
        );

        // 2. With CORS disabled
        let (_tx, rx2) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app_cors_disabled = create_router(AppState::new(rx2), true, false, false);
        let res2 = app_cors_disabled
            .oneshot(
                Request::builder()
                    .uri("/json/v2")
                    .header("Origin", "http://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(res2.headers().get("access-control-allow-origin").is_none());
    }

    #[tokio::test]
    async fn test_start_server_zero_port_bypass() {
        let sample = TosuV2Packet::default();
        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));

        // Even with an invalid or bound port, if both are false, it should succeed immediately
        let res = start_server("127.0.0.1", 0, false, false, true, None, rx).await;
        assert!(res.is_ok());
    }

    // ---- browser overlays ----

    fn temp_overlay_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("rtosu-server-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create overlay root");
        root
    }

    fn write_overlay_file(root: &std::path::Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create overlay parent");
        }
        std::fs::write(path, content).expect("write overlay file");
    }

    fn overlay_app(root: std::path::PathBuf) -> Router {
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        create_router(AppState::with_overlays(rx, root), true, true, true)
    }

    async fn get_text(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn test_overlay_dashboard_lists_discovered_overlays() {
        let root = temp_overlay_root("dashboard");
        write_overlay_file(
            &root,
            "Team Bar/index.html",
            "<!DOCTYPE html><html><head></head><body></body></html>",
        );
        write_overlay_file(
            &root,
            "Team Bar/metadata.txt",
            "Name: Team Bar\nAuthor: tester\nResolution: 400x90\n",
        );

        let (status, body) = get_text(overlay_app(root.clone()), "/overlays").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Team Bar"));
        assert!(body.contains("400x90"));
        // The served URL keeps the directory name verbatim, as tosu does.
        assert!(body.contains("/overlays/Team Bar/"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_entry_page_injects_the_shim_first() {
        let root = temp_overlay_root("inject");
        write_overlay_file(
            &root,
            "Bar/index.html",
            "<!DOCTYPE html><html><head><script src=\"index.js\"></script></head><body></body></html>",
        );

        let (status, body) = get_text(overlay_app(root.clone()), "/overlays/Bar/").await;
        assert_eq!(status, StatusCode::OK);

        let shim = body.find("overlay-shim.js").expect("shim must be injected");
        let overlay_script = body.find("index.js").expect("overlay script preserved");
        assert!(
            shim < overlay_script,
            "shim must run before overlay scripts"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_assets_are_served_with_their_content_type() {
        let root = temp_overlay_root("assets");
        write_overlay_file(&root, "Bar/index.html", "<html></html>");
        write_overlay_file(&root, "Bar/index.css", "body { color: red; }");

        let app = overlay_app(root.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar/index.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/css; charset=utf-8"
        );

        // A non-HTML asset must not be rewritten with the shim tag.
        let (_, body) = get_text(app, "/overlays/Bar/index.css").await;
        assert_eq!(body, "body { color: red; }");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_requests_cannot_escape_the_overlay_directory() {
        let root = temp_overlay_root("traversal");
        write_overlay_file(&root, "Bar/index.html", "<html>secret overlay</html>");
        write_overlay_file(&root, "secret.txt", "top secret");

        let app = overlay_app(root.clone());
        for uri in [
            "/overlays/Bar/../secret.txt",
            "/overlays/Bar/..%2Fsecret.txt",
            "/overlays/Bar/%2e%2e/secret.txt",
            "/overlays/Bar/..%5Csecret.txt",
        ] {
            let (status, body) = get_text(app.clone(), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must not resolve");
            assert!(!body.contains("top secret"), "{uri} leaked file content");
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_folder_url_redirects_to_a_trailing_slash() {
        let root = temp_overlay_root("redirect");
        write_overlay_file(&root, "Bar/index.html", "<html></html>");

        let response = overlay_app(root.clone())
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/overlays/Bar/"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_shim_asset_is_served_and_uncached() {
        let root = temp_overlay_root("shim");
        write_overlay_file(&root, "Bar/index.html", "<html></html>");

        let response = overlay_app(root.clone())
            .oneshot(
                Request::builder()
                    .uri("/overlays/__rtosu/overlay-shim.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );

        let (_, body) = get_text(
            overlay_app(root.clone()),
            "/overlays/__rtosu/overlay-shim.js",
        )
        .await;
        assert!(body.contains("__rtosuOverlayShim"));
        assert!(body.contains("'/ws': '/websocket/v2'"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_unknown_folder_is_not_found() {
        let root = temp_overlay_root("unknown");
        write_overlay_file(&root, "Bar/index.html", "<html></html>");

        let (status, _) = get_text(overlay_app(root.clone()), "/overlays/Nope/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_routes_are_absent_when_disabled() {
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let app = create_router(AppState::new(rx), true, true, true);

        for uri in ["/overlays", "/overlays/", "/overlays/Bar/"] {
            let (status, _) = get_text(app.clone(), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must not be routed");
        }
    }

    #[tokio::test]
    async fn test_overlay_picks_up_a_folder_dropped_in_later() {
        let root = temp_overlay_root("rescan");
        write_overlay_file(&root, "First/index.html", "<html>first</html>");

        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let app = create_router(AppState::with_overlays(rx, root.clone()), true, true, true);

        let (status, body) = get_text(app.clone(), "/overlays/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("First"));

        write_overlay_file(&root, "Second/index.html", "<html>second</html>");

        // The root directory mtime changed, so the store must rescan.
        let (status, body) = get_text(app, "/overlays/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Second"), "newly added overlay must appear");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_beatmap_background_rejects_a_foreign_mapset() {
        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 4242;

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        // Nothing is loaded, so any mapset request must be refused rather than
        // serving an unrelated image.
        let (status, _) = get_text(app.clone(), "/backgroundImage?mapset=999").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = get_text(app, "/files/beatmap/background").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_beatmap_background_resolves_relative_to_the_songs_folder() {
        let root = temp_overlay_root("background-relative");
        // tosu reports the background as a path relative to the songs folder.
        let songs = root.join("songs");
        let beatmap_dir = songs.join("123 Artist - Song");
        std::fs::create_dir_all(&beatmap_dir).unwrap();
        std::fs::write(beatmap_dir.join("bg.png"), [0x89, 0x50, 0x4E, 0x47]).unwrap();

        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 55;
        sample.folders.songs = songs.to_string_lossy().into_owned();
        sample.files.background = "bg.png".to_string();
        sample.direct_path.beatmap_background = "123 Artist - Song\\bg.png".to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        let (status, _) = get_text(app, "/files/beatmap/background").await;
        assert_eq!(status, StatusCode::OK);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_beatmap_background_serves_the_loaded_file() {
        let root = temp_overlay_root("background");
        let image = root.join("bg.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xD9]).expect("write image");

        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 77;
        sample.files.background = image.to_string_lossy().into_owned();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/backgroundImage?mapset=77")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );

        // The same route also answers the tosu v2 path.
        let (status, _) = get_text(app, "/files/beatmap/background").await;
        assert_eq!(status, StatusCode::OK);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_beatmap_background_falls_back_to_the_beatmap_folder() {
        let root = temp_overlay_root("background-fallback");
        let songs = root.join("songs");
        let dir = songs.join("123 Artist - Song");
        std::fs::create_dir_all(&dir).unwrap();
        // A ranked mapset background is named <set>-<id>.jpg.
        std::fs::write(dir.join("123-456.jpg"), [1, 2, 3]).unwrap();
        std::fs::write(dir.join("other.jpg"), [9, 9]).unwrap();

        // The memory read yielded nothing, so only the folder is usable.
        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 456;
        sample.folders.songs = songs.to_string_lossy().into_owned();
        sample.direct_path.beatmap_folder = "123 Artist - Song".to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/files/beatmap/background")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        // The <set>-<id> match wins over the other image in the folder.
        assert_eq!(bytes.as_ref(), &[1, 2, 3]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn background_fallback_rejects_a_folder_name_that_escapes_songs() {
        let songs = std::path::Path::new("D:/osu/Songs");
        assert!(find_beatmap_background(songs, "", 1).is_none());
        assert!(find_beatmap_background(songs, "   ", 1).is_none());
        assert!(find_beatmap_background(songs, "../secret", 1).is_none());
        assert!(find_beatmap_background(songs, "a/b", 1).is_none());
        assert!(find_beatmap_background(songs, "C:evil", 1).is_none());
    }

    #[tokio::test]
    async fn test_favicon_is_answered_without_a_body() {
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let app = create_router(AppState::new(rx), true, false, true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/favicon.ico")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_overlay_pages_are_revalidated_not_cached() {
        let root = temp_overlay_root("caching");
        write_overlay_file(&root, "Bar/index.html", "<html></html>");
        write_overlay_file(&root, "Bar/index.js", "console.log(1)");

        let app = overlay_app(root.clone());
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar/index.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let etag = first
            .headers()
            .get(header::ETAG)
            .expect("an ETag is required or browsers cache heuristically")
            .to_str()
            .unwrap()
            .to_string();

        // A matching validator must produce a cheap 304.
        let revalidated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar/index.js")
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);

        // Editing the file must invalidate the validator.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_overlay_file(&root, "Bar/index.js", "console.log(2)");
        let after_edit = app
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar/index.js")
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            after_edit.status(),
            StatusCode::OK,
            "edited overlay must be sent"
        );
        assert_ne!(
            after_edit
                .headers()
                .get(header::ETAG)
                .unwrap()
                .to_str()
                .unwrap(),
            etag
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_html_varies_on_host() {
        let root = temp_overlay_root("vary");
        write_overlay_file(&root, "Bar/index.html", "<html><head></head></html>");

        let app = overlay_app(root.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/overlays/Bar/")
                    .header(header::HOST, "192.168.1.50:24050")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::VARY).unwrap(), "Host");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );

        // The shim URL embeds the host, so a LAN load must not reuse a 127.0.0.1 one.
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("http://192.168.1.50:24050/overlays/__rtosu/overlay-shim.js"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn etag_comparison_handles_lists_wildcards_and_weak_forms() {
        let etag = "W/\"2a-ff\"";
        assert!(matches_etag(etag, etag));
        assert!(matches_etag("*", etag));
        assert!(matches_etag("W/\"2a-ff\"", etag));
        assert!(matches_etag("\"2a-ff\"", etag), "weak equals strong");
        assert!(matches_etag("W/\"old\", W/\"2a-ff\"", etag));
        assert!(!matches_etag("W/\"other\"", etag));
        assert!(!matches_etag("", etag));
    }

    #[test]
    fn query_value_reads_one_parameter() {
        assert_eq!(
            query_value(Some("mapset=42"), "mapset").as_deref(),
            Some("42")
        );
        assert_eq!(
            query_value(Some("a=1&mapset=42&b=2"), "mapset").as_deref(),
            Some("42")
        );
        assert_eq!(
            query_value(Some("mapset=1+2"), "mapset").as_deref(),
            Some("1 2")
        );
        assert_eq!(query_value(Some("mapset%3D1"), "mapset").as_deref(), None);
        assert_eq!(query_value(Some("other=1"), "mapset"), None);
        assert_eq!(query_value(None, "mapset"), None);
    }
}

use crate::overlays::{self, OverlayStore};
use crate::settings::{self, SettingsStore};
use crate::v2::TosuV2Packet;
use anyhow::{Context, Result};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::connect_info::{ConnectInfo, MockConnectInfo};
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use serde::Serialize;
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
    /// Whether an osu! process is attached right now.
    ///
    /// Not part of the payload, because tosu does not express "no game" in the
    /// payload -- it refuses to produce one. Every `/json*` route there throws when
    /// `getInstance(focusedClient)` is null and the route wrapper answers
    /// `500` with `{"error": message}`
    /// (`packages/server/utils/http.ts:186-207`), and every socket's loop skips
    /// its send (`packages/server/utils/socket.ts`, `if (!osuInstance ||
    /// clients.size === 0)`).
    ///
    /// So this flag reproduces the transport instead of inventing a body: an
    /// overlay pointed at rtosu sees the same `500` tosu's would produce, and a
    /// socket subscriber goes quiet rather than being handed a payload full of
    /// values tosu would never emit.
    pub attached: bool,
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
                attached: true,
            }),
            Err(err) => {
                tracing::error!("failed to serialize packet: {err:#}");
                None
            }
        }
    }

    /// The same payload, marked as having no game behind it.
    pub fn detached(mut self) -> Self {
        self.attached = false;
        self
    }

    pub fn default_packet() -> Self {
        let packet = TosuV2Packet::default();
        let json = serde_json::to_vec(&packet).unwrap_or_else(|_| b"{}".to_vec());
        Self {
            packet: Arc::new(packet),
            json: Bytes::from(json),
            attached: true,
        }
    }
}

/// tosu's answer when no osu! instance is running, verbatim.
///
/// Every one of tosu's four `/json*` routes has the same guard and the same
/// message -- `router/index.ts:44-46` (`/json`), `router/v2.ts:9-15` (`/json/v2`)
/// and `:17-23` (`/json/v2/precise`), `router/scApi.ts:6-12` (`/json/sc`) -- and
/// the thrown `Error` is turned into `500` plus `sendJson(res, { error: message })`
/// by the route wrapper (`utils/http.ts:186-207`), with the message also set as
/// the HTTP status message, URI-encoded.
///
/// The body is `application/json` because that is what `sendJson` sets
/// (`utils/index.ts`), and it is a JSON object rather than plain text: an earlier
/// reading of this contract had the body as plain text, which is the shape
/// `utils/http.ts:450` uses for a *different* failure and not the one these
/// routes take.
fn not_ready() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"osu is not ready/running"}"#,
    )
        .into_response()
}

#[derive(Clone)]
pub struct AppState {
    pub packet_rx: watch::Receiver<PublishedPacket>,
    /// User-supplied browser overlays. `None` disables the overlay routes.
    pub overlays: Option<Arc<OverlayStore>>,
    /// The settings page's store. `None` renders the landing page without a
    /// settings form and answers `/api/settings` with `404`.
    pub settings: Option<Arc<SettingsStore>>,
    /// The address the listener is bound to, for the landing page header.
    pub bound_host: String,
    pub bound_port: u16,
    /// Raised by `POST /api/restart`. `None` answers that route with `409`:
    /// only the CLI `serve` path wires a receiver that can re-exec, so a
    /// library embedder that never wired one must not see a request that
    /// silently does nothing.
    pub restart: Option<RestartSignal>,
}

/// A one-shot request to restart the process, wired by the CLI `serve` path.
///
/// The endpoint never restarts anything itself: it raises this flag, the
/// graceful shutdown closes the listener, and the CLI loop -- the only code
/// that owns the original command line -- spawns the replacement process.
/// That keeps "restart" out of the server task, which cannot re-exec itself
/// without leaving the port bound by the dying process behind.
#[derive(Clone, Debug)]
pub struct RestartSignal(watch::Sender<bool>);

impl RestartSignal {
    /// The signal to hand the server, and the receiver the CLI loop waits on.
    pub fn channel() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (Self(tx), rx)
    }

    /// Raise the restart request. Idempotent.
    pub fn request(&self) {
        let _ = self.0.send(true);
    }
}

impl AppState {
    pub fn new(packet_rx: watch::Receiver<PublishedPacket>) -> Self {
        Self {
            packet_rx,
            overlays: None,
            settings: None,
            bound_host: String::new(),
            bound_port: 0,
            restart: None,
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
            settings: None,
            bound_host: String::new(),
            bound_port: 0,
            restart: None,
        }
    }

    /// Attach the settings store and the bound address, so `/` can render the
    /// settings form and name the address it is serving on.
    pub fn with_settings(
        mut self,
        settings: Arc<SettingsStore>,
        bound_host: impl Into<String>,
        bound_port: u16,
    ) -> Self {
        self.settings = Some(settings);
        self.bound_host = bound_host.into();
        self.bound_port = bound_port;
        self
    }

    /// Wire the restart request so `POST /api/restart` can be honoured.
    pub fn with_restart(mut self, restart: RestartSignal) -> Self {
        self.restart = Some(restart);
        self
    }
}

/// Which payload `GET /json` serves. See `config::ServerConfig::json_payload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonPayload {
    /// The gosumemory-compatible payload, which is what tosu serves at `/json`.
    #[default]
    V1,
    /// rtosu's v2 payload, which is what `/json` served before the parity work.
    V2,
}

impl JsonPayload {
    /// Parse the config value, warning and falling back to v1 on anything else.
    ///
    /// A typo must not silently serve the shape the operator was trying to avoid,
    /// so an unrecognised value is a warning rather than a silent default.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "v1" | "" => Self::V1,
            "v2" => Self::V2,
            other => {
                tracing::warn!(
                    "server.json_payload = {other:?} is not a known payload; \
                     expected \"v1\" (tosu's choice for /json) or \"v2\". \
                     Falling back to v1."
                );
                Self::V1
            }
        }
    }
}

pub fn create_router(
    state: AppState,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
) -> Router {
    create_router_with(
        state,
        enable_http,
        enable_ws,
        cors_allow_all,
        JsonPayload::V1,
    )
}

/// Build the router, with `/json`'s payload shape chosen by the operator.
///
/// `json_payload` only affects `/json`. `/json/v2` always serves v2 and `/json/v1`
/// always serves v1, so a consumer can always reach either shape explicitly no
/// matter what this is set to.
pub fn create_router_with(
    state: AppState,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
    json_payload: JsonPayload,
) -> Router {
    let mut router = Router::new();

    if enable_http {
        router = router
            .route("/json/v2", get(handle_json_v2))
            .route("/json/v2/precise", get(handle_json_v2_precise))
            .route("/json/v1", get(handle_json_v1))
            // The StreamCompanion payload, for overlays written against
            // StreamCompanion rather than against tosu. Flat and 136 keys, so
            // nothing else in this router can be confused with it.
            .route("/json/sc", get(handle_json_sc))
            .route("/health", get(handle_health))
            // The landing page and its API. Registered inside the HTTP block so
            // the zero-port bypass still means what it says: with HTTP off,
            // there is no page either.
            .route("/", get(handle_landing))
            .route(
                "/api/settings",
                get(handle_settings_get)
                    .post(handle_settings_post)
                    // The only legal body is a few hundred bytes of
                    // `section.key: value` pairs, so a larger one is refused
                    // before it is parsed.
                    .layer(DefaultBodyLimit::max(settings::MAX_PATCH_BYTES)),
            )
            // Hot restart for restart-required settings, CLI `serve` only. No
            // body at all; the guard is in the handler.
            .route("/api/restart", post(handle_restart))
            // tosu file endpoints that overlays use to display the current
            // beatmap background, so drop-in overlays render unchanged.
            .route("/files/beatmap/background", get(handle_beatmap_background))
            // The shim rewrites /Songs/ onto /files/beatmap/ and passes
            // /files/skin/ through, so both destinations have to exist. See K-01.
            .route("/files/beatmap/{*path}", get(handle_songs_file))
            .route("/Songs/{*path}", get(handle_songs_file))
            .route("/files/skin/{*path}", get(handle_skin_file))
            .route("/backgroundImage", get(handle_beatmap_background))
            // Overlay pages trigger this automatically; answer it so the
            // browser console stays free of spurious 404s.
            .route("/favicon.ico", get(handle_favicon));
    }

    if enable_ws {
        router = router
            .route("/websocket/v2", get(handle_ws_upgrade))
            .route("/websocket/v2/precise", get(handle_ws_upgrade_precise))
            // tosu's v1 socket, serving the same shape as `/json/v1`. The overlay
            // shim passes `/ws` straight through to here.
            .route("/ws", get(handle_ws_upgrade_v1))
            // tosu's StreamCompanion wrapper: `/tokens` is `WS_SC`, whose
            // `stateFunctionName` is `getStateSC` -- the **SC** payload, not v2
            // (`tosu-sourcecode/packages/server/index.ts:38-42`,
            // `instances/index.ts:243-245`). tosu's own AGENTS.md calls it "a
            // compatibility endpoint specifically designed for overlays built for
            // StreamCompanion". The `:` and the array-shorthand are tosu's, and
            // the filter language is shared with the other sockets.
            .route("/tokens", get(handle_ws_upgrade_tokens))
            // tosu's command channel: `WS_COMMANDS` is constructed with an empty
            // `pollRateFieldName` and `stateFunctionName`
            // (`packages/server/index.ts:58-63`), so it **sends no data frames** --
            // it only delivers `applyFilters` and answers commands. Serving v2
            // here would be a superset no client asked for.
            .route("/websocket/commands", get(handle_ws_upgrade_commands));

        // `/json` is the one route whose payload the operator can choose.
        //
        // tosu serves the gosumemory-compatible payload at `/json`
        // (`packages/server/router/index.ts:43-53`) and the v2 payload at
        // `/json/v2`, so v1 is the default here -- but rtosu served v2 on this
        // path until the parity work, and a drop-in that answers a different
        // shape on the same URL with no error signal breaks an existing consumer
        // silently. `server.json_payload = "v2"` puts it back; `/json/v1` and
        // `/json/v2` are unaffected either way.
        //
        // The choice is made on the `Router` rather than inside a single
        // `get(...)` because the two handlers have distinct `impl Future` return
        // types, which cannot be unified behind one call.
        router = if json_payload == JsonPayload::V2 {
            router.route("/json", get(handle_json_v2))
        } else {
            router.route("/json", get(handle_json_v1))
        };
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
    let published = state.packet_rx.borrow();
    if !published.attached {
        return not_ready();
    }
    json_response(published.json.clone())
}

/// tosu's `/json/v2/precise` (`packages/server/router/v2.ts:21-31`).
///
/// `{keys, hitErrors, tourney}` and nothing else. This used to be the full v2
/// handler, so a precise client received the whole payload -- five graph series
/// included -- on every tick, with neither `keys` nor `hitErrors` present. Live
/// measurement: 221 bytes from tosu against 24,039 from rtosu on the same map in
/// the same state, and the endpoint whose entire reason to exist is a cheap
/// high-frequency feed.
///
/// Built per request rather than pre-encoded per poll, for the same reason as
/// v1 and SC: the precise payload is a view over data the poll already produced,
/// and pre-encoding it would add a serialisation to every tick for a route that
/// most consumers never open. The `hitErrors` array is an `Arc` clone, so the
/// frame does not copy the list.
async fn handle_json_v2_precise(State(state): State<AppState>) -> Response {
    // The `Arc` is cloned out and the guard dropped before the build, for the
    // reason given on `handle_json_v1`.
    let (attached, packet) = {
        let published = state.packet_rx.borrow();
        (published.attached, Arc::clone(&published.packet))
    };
    if !attached {
        return not_ready();
    }
    let precise = crate::v2::TosuPrecisePacket::from_v2(&packet);
    match serde_json::to_vec(&precise) {
        Ok(json) => json_response(Bytes::from(json)),
        Err(error) => {
            tracing::error!("failed to build the precise payload: {error}");
            server_error("failed to build the precise payload")
        }
    }
}

/// Serve the gosumemory-compatible payload.
///
/// This is tosu's `/json` (`packages/server/router/index.ts:43-53`), and it is
/// what `/json` serves here. `/json/v1` is an alias for the same payload;
/// `/json/v2` is the only path to the v2 shape.
///
/// The v1 payload is built per request rather than pre-encoded by the poll loop,
/// because it includes the strain graph and pre-encoding it would add a
/// serialisation to every tick for a route almost nothing calls. See
/// `validations/audits/audit-1.0.5.md` `L-07`.
async fn handle_json_v1(State(state): State<AppState>) -> Response {
    // Clone the `Arc` out from under the guard and drop it before building. A
    // `watch::Ref` holds the read lock for its whole lifetime, so holding one
    // across `from_v2` **and** `to_vec` blocks the poll loop's `tx.send` -- and
    // `tx.send` is what every WebSocket client is waiting on. One slow `/json`
    // request would stall the whole broadcast.
    //
    // `handle_json_v2` never had this problem, because the payload arrives
    // pre-encoded and the handler only bumps a refcount.
    let (attached, packet) = {
        let published = state.packet_rx.borrow();
        (published.attached, Arc::clone(&published.packet))
    };
    if !attached {
        return not_ready();
    }
    let v1 = crate::v1::GosuCompatibleApi::from_v2(&packet);
    match serde_json::to_vec(&v1) {
        Ok(json) => json_response(Bytes::from(json)),
        Err(error) => {
            tracing::error!("failed to build the v1 payload: {error}");
            server_error("failed to build the v1 payload")
        }
    }
}

/// Serve the StreamCompanion payload, tosu's `/json/sc`
/// (`tosu-sourcecode/packages/server/router/scApi.ts:5`).
///
/// Built per request for the same reason as v1: pre-encoding it would add a
/// serialisation to every poll tick for a route almost nothing calls.
///
/// Like the other three `/json*` routes, it answers `500 {"error":"osu is not
/// ready/running"}` when no client is attached -- tosu's guard is the same on all
/// four (`router/scApi.ts:9-11`), and making SC the only route that served a
/// stale `200` would have been worse than leaving the choice open. See
/// `validations/audits/audit-1.0.5.md` `M-08`.
async fn handle_json_sc(State(state): State<AppState>) -> Response {
    // The `Arc` is cloned out and the guard dropped before the build, for the
    // reason given on `handle_json_v1`.
    let (attached, packet) = {
        let published = state.packet_rx.borrow();
        (published.attached, Arc::clone(&published.packet))
    };
    if !attached {
        return not_ready();
    }
    let sc = crate::sc::ScPayload::from_v2(&packet);
    match serde_json::to_vec(&sc) {
        Ok(json) => json_response(Bytes::from(json)),
        Err(error) => {
            tracing::error!("failed to build the SC payload: {error}");
            server_error("failed to build the SC payload")
        }
    }
}

fn server_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

/// Upgrade handler for the v1 socket, the tosu-compatible `/ws`.
async fn handle_ws_upgrade_v1(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::debug!("Incoming v1 WebSocket upgrade request");
    ws.on_upgrade(move |socket| handle_ws_stream_v1(socket, state.packet_rx))
}

/// Stream the v1 payload on each change.
///
/// Unlike the v2 stream this re-encodes per client per tick, because the v1 body
/// is not pre-computed. That is a deliberate trade: pre-encoding it would tax every
/// tick on the reader's hot path for a legacy endpoint, and v1 clients are rare.
/// A v1 consumer is paying roughly what it would have paid to re-encode for itself.
async fn handle_ws_stream_v1(
    mut socket: WebSocket,
    mut packet_rx: watch::Receiver<PublishedPacket>,
) {
    tracing::debug!("v1 WebSocket client connected");

    if let Some(frame) = v1_frame(&mut packet_rx)
        && socket.send(frame).await.is_err()
    {
        tracing::debug!("v1 WebSocket client disconnected during initial handshake");
        return;
    }

    // `biased` so a queued inbound frame is taken before the next data frame is
    // built; see `drain_inbound_frame`.
    loop {
        tokio::select! {
            biased;
            inbound = socket.recv() => {
                if !drain_inbound_frame(inbound, "/ws") {
                    break;
                }
            }
            changed = packet_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(frame) = v1_frame(&mut packet_rx) else {
                    continue;
                };
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::debug!("v1 WebSocket client disconnected");
}
/// Build, encode and wrap the v1 payload for the current published state.
///
/// `None` while no osu! is attached, which is how the streams go quiet: tosu's
/// socket loop skips its send entirely when `getInstance(focusedClient)` is null
/// (`packages/server/utils/socket.ts`, `if (!osuInstance || clients.size === 0)
/// { await sleep(500); continue; }`), so it never sends a frame at all in that
/// state -- not a stale one, and not an error frame either. Returning `None`
/// rather than an error value is what reproduces that, and it also means a
/// consumer never has to distinguish "no game" from a payload.
fn v1_frame(packet_rx: &mut watch::Receiver<PublishedPacket>) -> Option<Message> {
    // `borrow_and_update` marks the change as seen, and the `Ref` it returns must
    // not outlive the refcount bump: holding it across the build and the encode
    // blocks the poll loop's `tx.send`, so a slow frame would stall every other
    // WebSocket client. `split_packets` does the bump and returns owned data.
    let (attached, packet) = split_packets(packet_rx);
    if !attached {
        return None;
    }
    let v1 = crate::v1::GosuCompatibleApi::from_v2(&packet);
    let json = match serde_json::to_vec(&v1) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!("failed to build the v1 payload: {error}");
            return None;
        }
    };
    ws_text(Bytes::from(json))
}

/// Mark the current value as seen and hand back owned copies, so no `watch` guard
/// is alive while a caller builds or encodes anything.
///
/// This is the one rule every payload frame builder follows. `watch::Ref` holds
/// the read lock, and the poll loop's `tx.send` needs the write lock, so a guard
/// that spans a serialisation turns one slow consumer into a stall for every
/// consumer. The clone is of an `Arc`, so the guard's lifetime is over as soon as
/// this returns.
fn split_packets(packet_rx: &mut watch::Receiver<PublishedPacket>) -> (bool, Arc<TosuV2Packet>) {
    let published = packet_rx.borrow_and_update();
    (published.attached, Arc::clone(&published.packet))
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

/// Whether the request provably came from the loopback interface.
///
/// A request without connect info is **not** treated as local. A router served
/// without `into_make_service_with_connect_info` cannot say where a request came
/// from, and assuming "local" would silently disable the settings write guard.
fn peer_is_loopback(peer: PeerAddress) -> bool {
    peer.0.map(|addr| addr.ip().is_loopback()).unwrap_or(false)
}

/// The peer's address, when the service knows it.
///
/// Hand-written rather than `Option<ConnectInfo<SocketAddr>>` because axum only
/// supports optional extraction for the extractors that implement
/// `OptionalFromRequestParts`, and `ConnectInfo` is not one of them: as an
/// extractor its rejection is `500`, so the settings routes would answer "500"
/// instead of "403" whenever connect info is unavailable. Reading the extension
/// directly makes "unknown peer" a value the guard handles.
///
/// Both halves are read: real serving inserts `ConnectInfo`, and
/// [`MockConnectInfo`] -- which is how tests set a peer without a socket --
/// inserts itself.
#[derive(Clone, Copy, Debug)]
struct PeerAddress(Option<SocketAddr>);

impl<S> FromRequestParts<S> for PeerAddress
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| *addr)
            .or_else(|| {
                parts
                    .extensions
                    .get::<MockConnectInfo<SocketAddr>>()
                    .map(|MockConnectInfo(addr)| *addr)
            });
        Ok(Self(peer))
    }
}

/// `{"error": message}`, which is tosu's error shape everywhere
/// (`utils/http.ts:186-207`) and the one rtosu already reproduces in
/// [`not_ready`], so a consumer that parses one parses all of them.
fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// A serialisable value as the response body.
fn json_serializable<T: Serialize>(value: T) -> Response {
    match serde_json::to_vec(&value) {
        Ok(bytes) => json_response(Bytes::from(bytes)),
        Err(err) => {
            tracing::error!("failed to serialise a response: {err:#}");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to encode the response",
            )
        }
    }
}

/// Whether the request declares a JSON body.
///
/// A missing or different content type is refused rather than guessed at: the
/// patch is applied verbatim, so the client has to say what it is sending.
fn content_type_is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

/// `GET /`: the settings landing page.
///
/// Renders with whatever is attached: the settings form and overlay cards when
/// both are available, and a notice in place of either when it is not.
async fn handle_landing(State(state): State<AppState>, peer: PeerAddress) -> Response {
    let found = match state.overlays.as_ref() {
        Some(store) => Some((store.list().await, store.root.clone())),
        None => None,
    };
    let settings = state
        .settings
        .as_ref()
        .map(|store| settings::settings_response(store, peer_is_loopback(peer)));
    let overlays = found
        .as_ref()
        .map(|(overlays, root)| (overlays.as_slice(), root.as_path()));

    let html = settings::landing_html(&settings::LandingView {
        host: &state.bound_host,
        port: state.bound_port,
        config_path: state.settings.as_ref().map(|store| store.path()),
        settings,
        overlays,
        // Same rule as the endpoint itself: the button only shows where it
        // would actually work.
        can_restart: state.restart.is_some() && state.settings.is_some() && peer_is_loopback(peer),
    });
    text_response(html, "text/html; charset=utf-8")
}

/// `GET /api/settings`: the current configuration plus what the page needs to
/// render it, including whether this viewer may change anything.
async fn handle_settings_get(State(state): State<AppState>, peer: PeerAddress) -> Response {
    let Some(store) = state.settings.as_ref() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "settings are unavailable in this mode",
        );
    };
    json_serializable(settings::settings_response(store, peer_is_loopback(peer)))
}

/// `POST /api/settings`: validate a flat patch of `section.key` values, write it
/// to the config file, then publish the part of it the poll loop can adopt
/// without a restart.
///
/// **Atomic or nothing.** Every check runs before the file is touched, and the
/// only step that changes anything in memory follows the write, so a rejected
/// patch leaves both the file and the running process exactly as they were.
async fn handle_settings_post(
    State(state): State<AppState>,
    peer: PeerAddress,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(store) = state.settings.as_ref() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "settings are unavailable in this mode",
        );
    };

    if !content_type_is_json(&headers) {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected Content-Type: application/json",
        );
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    match settings::write_blocker(
        &store.config(),
        peer_is_loopback(peer),
        origin,
        store.check_writable(),
    ) {
        Some(settings::WriteBlocker::SettingsWriteLocalOnly) => {
            return json_error(
                StatusCode::FORBIDDEN,
                "settings writes are restricted to localhost",
            );
        }
        // A change that cannot be persisted is refused rather than accepted and
        // lost on the next restart.
        Some(settings::WriteBlocker::ConfigReadOnly) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "the config file could not be opened for writing; nothing was changed",
            );
        }
        None => {}
    }

    let patch: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(patch) => patch,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("the request body is not valid JSON: {err}"),
            );
        }
    };

    let outcome = match store.apply_patch(&patch) {
        Ok(outcome) => outcome,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, &format!("{err:#}")),
    };

    if let Err(err) = store.persist(&outcome.config) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("writing the config file failed: {err:#}"),
        );
    }
    if let Err(err) = store.commit(outcome.config.clone()) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("applying the configuration failed: {err:#}"),
        );
    }

    tracing::info!(
        "settings updated from {}: {}",
        match peer.0 {
            Some(addr) => addr.to_string(),
            None => "an unknown peer".to_string(),
        },
        outcome.applied.join(", ")
    );

    json_serializable(serde_json::json!({
        "ok": true,
        "applied": outcome.applied,
        "restart_required": outcome.restart_required,
        "config": outcome.config,
    }))
}

/// `POST /api/restart`: ask the CLI loop to replace this process with a fresh
/// one started from the same command line.
///
/// The guard is **stricter than the settings write guard, and deliberately
/// not configurable**: a restart is at least as dangerous as a config write --
/// it kills every live overlay socket the moment it lands -- so it always
/// requires a loopback peer and a loopback `Origin`, even when
/// `settings_write_local_only` is off. With no receiver wired (`None`) the
/// route answers `409`: a request nobody will ever honour should say so.
///
/// The signal fires after a short delay so this response reaches the browser
/// before the listener starts closing.
async fn handle_restart(
    State(state): State<AppState>,
    peer: PeerAddress,
    headers: HeaderMap,
) -> Response {
    if state.settings.is_none() {
        return json_error(
            StatusCode::NOT_FOUND,
            "settings are unavailable in this mode",
        );
    }
    let Some(restart) = state.restart.clone() else {
        return json_error(
            StatusCode::CONFLICT,
            "no process supervisor is attached; this instance cannot restart itself",
        );
    };
    if !peer_is_loopback(peer) {
        return json_error(
            StatusCode::FORBIDDEN,
            "restarts are restricted to the machine running the server",
        );
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && !settings::origin_is_loopback(origin)
    {
        return json_error(
            StatusCode::FORBIDDEN,
            "restarts are restricted to the machine running the server",
        );
    }

    tracing::info!(
        "restart requested from {}; a new process will take over on this port",
        match peer.0 {
            Some(addr) => addr.to_string(),
            None => "an unknown peer".to_string(),
        }
    );
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        restart.request();
    });

    Json(serde_json::json!({
        "ok": true,
        "restarting": true,
        "note": "a new process is starting on this port; reload this page in a few seconds",
    }))
    .into_response()
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
    // try that first, fall back to the raw value, and finally to the files
    // sitting in the beatmap folder itself.
    let songs_root = canonical_songs_root(&songs);
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(root) = songs_root.as_deref()
        && !relative.trim().is_empty()
    {
        candidates.push(root.join(relative.trim()));
    }
    if !background.trim().is_empty() {
        candidates.push(std::path::PathBuf::from(background.trim()));
    }

    // A background is only ever served from inside the songs folder, so the
    // fallback goes through the same gate as the memory-read candidates rather
    // than carrying a second, weaker copy of the rule.
    let path = songs_root.as_deref().and_then(|root| {
        let fallback = find_beatmap_background(root, &folder, loaded_set);
        contained_in_songs(
            root,
            candidates
                .iter()
                .chain(fallback.iter())
                .map(|candidate| candidate.as_path()),
        )
    });

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

/// Serve a file from inside the songs folder, at the path the overlay asked for.
///
/// tosu exposes the whole Songs tree at `/Songs/*` (`router/v1.ts:5`) and again at
/// `/files/beatmap/*` (`router/v2.ts:39-59`), and the overlay shim rewrites
/// `/Songs/` onto `/files/beatmap/`. rtosu registered neither destination, so
/// every overlay asking for a beatmap preview, the `.osu` file or any other song
/// asset got a 404 with an empty body. See `K-01` in validations/audits/audit-1.0.5.md.
///
/// Range requests are not handled yet. tosu streams 206/416 for these paths
/// (`utils/directories.ts:129-152`), which is what lets an overlay's `<audio>`
/// element seek; that is a separate change from serving the file at all.
async fn handle_songs_file(State(state): State<AppState>, Path(rel): Path<String>) -> Response {
    // Copy out of the watch guard before awaiting: the guard is not `Send` and
    // handler futures must be.
    let songs = {
        let published = state.packet_rx.borrow();
        published.packet.folders.songs.clone()
    };
    serve_contained_file(canonical_songs_root(&songs), &rel, "song file").await
}

/// Serve a file from inside the skin folder.
///
/// tosu serves this at `/files/skin/*` for stable clients and throws for lazer
/// (`router/v2.ts:61-96`, `:82-86`); rtosu reads stable only, so the stable
/// behaviour is the whole of it.
async fn handle_skin_file(State(state): State<AppState>, Path(rel): Path<String>) -> Response {
    let skin = {
        let published = state.packet_rx.borrow();
        published.packet.folders.skin.clone()
    };
    serve_contained_file(canonical_songs_root(&skin), &rel, "skin file").await
}

/// Resolve `relative` under `root` and serve it, or 404 with a body.
///
/// The body matters: an empty 404 is axum's unmatched-path fallback, which is how
/// `every_file_route_the_shim_advertises_is_registered` tells "this route exists but
/// the file is not there" from "this route does not exist at all".
async fn serve_contained_file(
    root: Option<std::path::PathBuf>,
    relative: &str,
    missing: &'static str,
) -> Response {
    let requested = relative.trim();
    let Some(root) = root.filter(|_| !requested.is_empty()) else {
        return not_found(missing);
    };

    let joined = root.join(requested);
    let Some(path) = contained_in_songs(&root, [joined.as_path()]) else {
        tracing::debug!("requested file is not inside the served folder: {requested}");
        return not_found(missing);
    };

    let content_type = overlays::content_type_for(&path);
    match overlays::read_file(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                // Song and skin assets are addressed by a path that is reused
                // across maps, so a cached copy would outlive the map it came from.
                (header::CACHE_CONTROL, "no-store"),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(err) => {
            tracing::debug!("serving {requested} failed: {err:#}");
            not_found(missing)
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
    let body = Body::from(overlays::overlay_shim_js());
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
    // Axum hands over the already-decoded segment, so it is re-encoded on the
    // way out: a folder named `Team Bar` can only be pointed at by a `Location`
    // of `/overlays/Team%20Bar/`, because a raw space is not a legal
    // `Location` value and a bare `%` in a folder name would be misread.
    let target = format!(
        "{}/{}/",
        overlays::OVERLAYS_BASE,
        overlays::percent_encode(&slug)
    );
    Redirect::permanent(&target).into_response()
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

/// Resolve the songs folder into the canonical root that background files must
/// stay inside.
///
/// Both sessions always publish `folders.songs`, so an empty value means the
/// packet is not populated yet. This deliberately fails closed in that case:
/// containment needs a root to be contained by, and guessing one would turn a
/// missing field into an unconstrained file server.
fn canonical_songs_root(songs: &str) -> Option<std::path::PathBuf> {
    let trimmed = songs.trim();
    if trimmed.is_empty() {
        return None;
    }

    std::fs::canonicalize(trimmed)
        .ok()
        .filter(|root| root.is_dir())
}

/// Pick the first candidate that resolves to a real file inside `songs`.
///
/// Canonicalization is the rule, not string matching: it collapses `..` and
/// symlinks, so a candidate that walks out of the songs folder and back in is
/// accepted while one that ends outside is rejected. A candidate that does not
/// exist fails to canonicalize and is skipped, which is also how the old
/// `is_file()` check is expressed. The canonical path is what gets returned, so
/// whatever is served is the path that was actually validated.
fn contained_in_songs<'a>(
    songs_root: &std::path::Path,
    candidates: impl IntoIterator<Item = &'a std::path::Path>,
) -> Option<std::path::PathBuf> {
    candidates.into_iter().find_map(|candidate| {
        let canonical = std::fs::canonicalize(candidate).ok()?;
        (canonical.starts_with(songs_root) && canonical.is_file()).then_some(canonical)
    })
}

/// Locate a beatmap background image inside its songs folder.
///
/// `files.background` comes from a memory read that does not always yield a
/// filename, and `direct_path.beatmap_background` can name the beatmap folder
/// rather than an image. A beatmap folder normally holds exactly one image, so
/// fall back to osu!'s conventional names and then to a single image in the
/// folder. Only the immediate folder is inspected, and only when it sits inside
/// the songs folder. Containment is not decided here: the caller re-checks the
/// returned path against the canonical songs root.
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

/// tosu's `/websocket/v2/precise`: the same three-key payload as
/// `/json/v2/precise`, streamed.
///
/// This used to be the full v2 stream, so a precise subscriber paid the whole
/// packet -- including the strain graph -- on every tick. The rate is still the
/// main loop's rather than tosu's 10 ms precise loop; that gap is `B-03` in
/// `validations/audits/audit-1.0.5.md` and is the threading restructure
/// `FIX-008` owns, not something to paper over here. What this fixes is the
/// payload, which is the part that was
/// wrong by two orders of magnitude.
async fn handle_ws_upgrade_precise(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::debug!("Incoming precise WebSocket upgrade request");
    ws.on_upgrade(move |socket| handle_ws_stream_precise(socket, state.packet_rx))
}

/// Stream the precise payload on each change.
async fn handle_ws_stream_precise(
    mut socket: WebSocket,
    mut packet_rx: watch::Receiver<PublishedPacket>,
) {
    tracing::debug!("precise WebSocket client connected");

    if let Some(frame) = precise_frame(&mut packet_rx)
        && socket.send(frame).await.is_err()
    {
        tracing::debug!("precise WebSocket client disconnected during initial handshake");
        return;
    }

    // `biased` so a queued inbound frame is taken before the next data frame is
    // built; see `drain_inbound_frame`.
    loop {
        tokio::select! {
            biased;
            inbound = socket.recv() => {
                if !drain_inbound_frame(inbound, "/websocket/v2/precise") {
                    break;
                }
            }
            changed = packet_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(frame) = precise_frame(&mut packet_rx) else {
                    continue;
                };
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::debug!("precise WebSocket client disconnected");
}

/// The one inbound rule every data socket shares: a frame that arrives from a
/// data socket is **discarded**, and only a close or a transport error ends the
/// connection.
///
/// tosu reads messages on every socket -- `ws.on('message', data =>
/// this.onMessageCallback(data.toString(), ws, this))` in
/// `packages/server/utils/socket.ts:36-41` -- and v1, v2 and the precise socket
/// pass `handleSocketCommands`, while `/ws` and `/tokens` only get the filter
/// reader. So all five consume their input; none of them treat a message as a
/// reason to stop.
///
/// rtosu used to read nothing at all on four of the five, so a client that sent
/// anything left the frame queued on the transport: pong replies and the
/// keepalive an overlay's own library sends are the ordinary case, and a socket
/// that never reads a pong is a socket whose peer eventually gives up on it. The
/// fix is to read and drop, not to implement a command surface: the *only* socket
/// with a documented inbound contract in rtosu is the filter list, and that one
/// already had a reader.
///
/// `biased` matters for the same reason it does in the filter loop: the payload
/// is broadcast on a timer, so the data branch is ready on nearly every wakeup,
/// and an unbiased `select!` would let inbound frames sit behind it.
fn drain_inbound_frame(message: Option<Result<Message, axum::Error>>, pathname: &str) -> bool {
    match message {
        // Read and drop. Pings are answered by axum's own machinery, so a
        // `Message::Ping` reaching here is already handled bookkeeping.
        Some(Ok(_)) => true,
        Some(Err(error)) => {
            tracing::debug!("websocket read error on {pathname}: {error}");
            false
        }
        None => false,
    }
}

/// Build, encode and wrap the precise payload for the current published state.
fn precise_frame(packet_rx: &mut watch::Receiver<PublishedPacket>) -> Option<Message> {
    // Silent while detached, like the other three streams and like tosu's own
    // loop; see `v1_frame`. The guard is also dropped before the build; see
    // `split_packets`.
    let (attached, packet) = split_packets(packet_rx);
    if !attached {
        return None;
    }
    let precise = crate::v2::TosuPrecisePacket::from_v2(&packet);
    match serde_json::to_vec(&precise) {
        Ok(json) => ws_text(Bytes::from(json)),
        Err(error) => {
            // A payload that will not encode must not silently stop the stream.
            tracing::error!("failed to build the precise payload: {error}");
            None
        }
    }
}

/// tosu's `/tokens` socket: the StreamCompanion wrapper.
///
/// The connection sends a filter list and then receives only those leaves of the
/// **SC** payload -- not v2, which is what the other sockets stream. Getting that
/// backwards is easy, because the filter feature belongs to the socket layer
/// generally, and it is why the live probe initially reported `{}` for filters
/// naming v2 keys: the SC payload is flat and has no `session` or `client`.
///
/// This is the one rtosu socket that **reads** from the client, which is what
/// makes it different from the other four
/// (`validations/audits/audit-1.0.5.md` `D-04`: "inbound messages are never read at all"). The read
/// is bounded -- one `select!` arm against the receiver, so a silent client costs
/// nothing extra -- and every message is length-capped, because a filter list
/// arrives from the network on a path that used to have no input at all.
async fn handle_ws_upgrade_tokens(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_tokens(socket, state.packet_rx, TokensRoute::Sc))
}

/// tosu's `/websocket/commands`.
///
/// **No data frames.** `WS_COMMANDS` is built with an empty `stateFunctionName`
/// (`packages/server/index.ts:58-63`), so tosu's loop has nothing to send and the
/// socket exists purely to carry commands -- `applyFilters` and the dashboard's
/// overlay-list refreshes. Streaming a payload here would send frames a client
/// written against tosu has no handler for.
async fn handle_ws_upgrade_commands(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_tokens(socket, state.packet_rx, TokensRoute::Commands))
}

/// Which payload a socket streams, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokensRoute {
    /// `/tokens`: the SC payload, filterable.
    Sc,
    /// `/websocket/commands`: no payload, commands only.
    Commands,
}

impl TokensRoute {
    fn pathname(self) -> &'static str {
        match self {
            TokensRoute::Sc => "/tokens",
            TokensRoute::Commands => "/websocket/commands",
        }
    }

    /// Whether this socket sends data frames at all.
    fn streams(self) -> bool {
        match self {
            TokensRoute::Sc => true,
            TokensRoute::Commands => false,
        }
    }
}

/// The filter socket's loop, shared by `/tokens` and `/websocket/commands`.
async fn handle_ws_tokens(
    mut socket: WebSocket,
    mut packet_rx: watch::Receiver<PublishedPacket>,
    route: TokensRoute,
) {
    let pathname = route.pathname();
    tracing::debug!("WebSocket client connected to {pathname}");
    let mut filters: Vec<crate::ws_filters::Filter> = Vec::new();

    // A command-only socket has no receiver to poll, so it never selects on one.
    // Sending the receiver in anyway would pin the last packet for the life of
    // the connection, which the broadcast path pays for on every send.
    if !route.streams() {
        serve_commands_only(&mut socket, pathname, &mut filters).await;
        tracing::debug!("WebSocket command client disconnected from {pathname}");
        return;
    }

    loop {
        // `biased` is load-bearing, not a style choice. The payload is broadcast
        // on a timer, so the data branch is ready on essentially every wakeup; an
        // unbiased `select!` picks uniformly among ready branches and the inbound
        // branch loses often enough that a filter could sit unapplied across
        // several ticks. With `biased` and `recv` first, a message that has
        // already arrived is always taken before the next frame is built, which
        // is also the order tosu applies it in -- its socket reads on the `message`
        // event, decoupled from the send loop (`utils/socket.ts:76-93`).
        tokio::select! {
            biased;
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(message)) => {
                        if !handle_token_message(&message, pathname, &mut filters) {
                            break;
                        }
                    }
                    // A close frame, or a transport error, ends the connection.
                    Some(Err(error)) => {
                        tracing::debug!("websocket read error on {pathname}: {error}");
                        break;
                    }
                    None => break,
                }
            }
            changed = packet_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                // The frame is built inside a scope that ends before the
                // `await`, because `borrow_and_update` holds a read guard and a
                // guard held across an await makes the whole future
                // non-`Send`. The existing handlers avoid this by cloning the
                // bytes first; the filter path cannot, because the bytes only
                // exist after the parse.
                let frame = {
                    // The `Arc` is cloned out and the guard dropped before the
                    // build. This stream is the worst case: the SC payload is
                    // built, filtered and re-encoded per frame **per client**, so
                    // a guard held across it would let one connected client stall
                    // the poll loop for every other client. See `split_packets`.
                    let (attached, packet) = split_packets(&mut packet_rx);
                    if !attached {
                        // Silent while no game is attached, as on every other
                        // stream; see `v1_frame`.
                        None
                    } else {
                        // The SC payload is built per frame, like `/json/sc`, so a
                        // filtered connection is the only thing paying for it.
                        let frame = if filters.is_empty() {
                            None
                        } else {
                            let json = sc_bytes(&packet);
                            match filter_frame(&filters, &json) {
                                Some(frame) => Some(frame),
                                // A payload that will not parse or will not encode must
                                // not silently stop the stream. Fall back to the full
                                // payload for this tick and keep the connection.
                                None => {
                                    tracing::warn!("filtering failed; sending the full payload");
                                    ws_text(sc_bytes(&packet))
                                }
                            }
                        };
                        match frame {
                            Some(frame) => Some(frame),
                            // Unfiltered: the whole SC payload, freshly built.
                            None => ws_text(sc_bytes(&packet)),
                        }
                    }
                };
                let Some(frame) = frame else { continue };
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::debug!("WebSocket client disconnected from {pathname}");
}

/// The command socket's loop: inbound frames only, no data frames.
async fn serve_commands_only(
    socket: &mut WebSocket,
    pathname: &str,
    filters: &mut Vec<crate::ws_filters::Filter>,
) {
    while let Some(Ok(message)) = socket.recv().await {
        if !handle_token_message(&message, pathname, filters) {
            break;
        }
    }
}

/// Serialise the SC payload, which is the one `/tokens` streams.
fn sc_bytes(packet: &TosuV2Packet) -> Bytes {
    serde_json::to_vec(&crate::sc::ScPayload::from_v2(packet))
        .map(Bytes::from)
        .unwrap_or_default()
}

/// Apply one inbound message. Returns `false` when the connection should close.
fn handle_token_message(
    message: &Message,
    pathname: &str,
    filters: &mut Vec<crate::ws_filters::Filter>,
) -> bool {
    let Message::Text(text) = message else {
        // tosu ignores binary and ping/pong frames. A ping is answered by axum's
        // own machinery, so only a close needs handling here.
        return !matches!(message, Message::Close(_));
    };
    // A filter list arrives from the network onto a socket that previously had no
    // input, so it is capped. tosu has no cap because it is a local overlay API;
    // 64 KiB is far above any real filter list and far below anything that could
    // be used to exhaust memory.
    const MAX_COMMAND_BYTES: usize = 64 * 1024;
    if text.len() > MAX_COMMAND_BYTES {
        tracing::warn!(
            "dropping an oversized {pathname} command: {} bytes",
            text.len()
        );
        return true;
    }

    let command = crate::ws_filters::normalize_socket_command(text.as_str(), pathname);
    match crate::ws_filters::filters_from_command(&command) {
        // tosu assigns `socket.filters` only on success (`commands.ts:161`), so a
        // rejected message leaves the previous list in place.
        Some(parsed) => {
            *filters = parsed;
            true
        }
        // Not a command we act on. tosu answers anything it did not handle with
        // `{command, message}` (`commands.ts:169-177`); a data socket must not
        // echo, so it is dropped.
        None => true,
    }
}

/// One filtered frame, or `None` if the payload cannot be filtered.
///
/// The frame is built with `Message::text` rather than routed through
/// [`ws_text`], because `serde_json` cannot produce invalid UTF-8 -- so a `None`
/// here unambiguously means "filtering failed" and the caller should fall back to
/// the full payload, with no risk of conflating it with the encoding failure
/// `ws_text` exists to handle.
fn filter_frame(filters: &[crate::ws_filters::Filter], json: &[u8]) -> Option<Message> {
    let data = crate::ws_filters::parse_packet(json)?;
    let bytes = crate::ws_filters::filter_json(filters, &data)?;
    // `serde_json` emits UTF-8 by construction, so the string conversion below
    // cannot fail; an error would mean a bug in the encoder, not bad input.
    let text = String::from_utf8(bytes).ok()?;
    Some(Message::text(text))
}

/// Wrap a serialized packet as a text frame, or `None` if it is not valid UTF-8.
///
/// This is on the WebSocket broadcast path, so a bad frame must cost one dropped
/// packet rather than the process. The release profile sets `panic = "abort"`,
/// so an `expect` here would take the server down with no unwinding and no
/// backtrace, mid-match, for every connected client at once.
fn ws_text(json: Bytes) -> Option<Message> {
    match Utf8Bytes::try_from(json) {
        Ok(text) => Some(Message::Text(text)),
        Err(error) => {
            tracing::error!("dropping websocket frame: serialized packet is not utf-8: {error}");
            None
        }
    }
}

async fn handle_ws_stream(mut socket: WebSocket, mut packet_rx: watch::Receiver<PublishedPacket>) {
    tracing::debug!("WebSocket client connected");

    // Send immediate initial state, unless there is no game: tosu's socket loop
    // skips its send entirely with no instance, so it never opens with a frame
    // either. See `v2_frame`.
    match v2_frame(&mut packet_rx) {
        Some(frame) => {
            if socket.send(frame).await.is_err() {
                tracing::debug!("WebSocket client disconnected during initial handshake");
                return;
            }
        }
        None => tracing::debug!("no initial packet: osu! is not attached"),
    }

    // Stream updates on each tick. The payload was encoded once by the poll
    // loop, so this is a refcount bump and a write, not a re-serialization.
    //
    // `biased` so a queued inbound frame is taken before the next data frame is
    // built; see `drain_inbound_frame`.
    loop {
        tokio::select! {
            biased;
            inbound = socket.recv() => {
                if !drain_inbound_frame(inbound, "/websocket/v2") {
                    break;
                }
            }
            changed = packet_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(frame) = v2_frame(&mut packet_rx) else {
                    continue;
                };
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::debug!("WebSocket client disconnected");
}

/// The pre-encoded v2 bytes for the current published state, or `None` while no
/// osu! is attached.
///
/// This has to be an `Option` and the caller has to skip on `None`, the way the
/// other three streams already do. Substituting an empty `Bytes` instead is not
/// equivalent: empty bytes are valid UTF-8, so `ws_text` turns them into a
/// zero-length `Message::Text("")` and sends it. Every consumer of this socket --
/// both bundled overlays included -- calls `JSON.parse(event.data)` on the frame,
/// so that takes a `SyntaxError` at exactly the moment the game exits, which is
/// the between-maps restart in a tournament. tosu sends nothing in that state
/// (`utils/socket.ts`, `if (!osuInstance || clients.size === 0)`), so silence is
/// also the faithful answer.
fn v2_frame(packet_rx: &mut watch::Receiver<PublishedPacket>) -> Option<Message> {
    // This one is cheap enough that the guard could stay: it only clones
    // pre-encoded bytes and never builds anything. `borrow_and_update` still has
    // to happen, so the read lock is taken and released immediately.
    let published = packet_rx.borrow_and_update();
    if !published.attached || published.json.is_empty() {
        return None;
    }
    ws_text(published.json.clone())
}

/// Start the tosu-compatible HTTP and WebSocket server.
/// If both `enable_http` and `enable_ws` are false, the function immediately
/// returns `Ok(())` without binding any TCP port (zero-port bypass).
/// `overlays_dir` enables user-supplied browser overlays when set, and
/// `settings` enables the landing page's settings form and its API.
/// `restart` wires `POST /api/restart`; with `None` the route answers `409`.
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
    json_payload: JsonPayload,
    settings: Option<Arc<SettingsStore>>,
    restart: Option<RestartSignal>,
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
        json_payload,
        settings,
        restart,
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
///
/// The peer address is part of the service: `settings_write_local_only` refuses
/// a settings write from anywhere but the loopback interface, which cannot be
/// decided without `ConnectInfo`.
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_listener(
    listener: tokio::net::TcpListener,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
    overlays_dir: Option<std::path::PathBuf>,
    packet_rx: watch::Receiver<PublishedPacket>,
    json_payload: JsonPayload,
    settings: Option<Arc<SettingsStore>>,
    restart: Option<RestartSignal>,
) -> Result<()> {
    let bound = listener.local_addr().ok();
    let mut state = match overlays_dir {
        Some(root) => AppState::with_overlays(packet_rx, root),
        None => AppState::new(packet_rx),
    };
    if let Some(store) = settings {
        // `local_addr` is the address actually bound, so a configured port of
        // `0` shows the port the OS picked rather than `0`.
        state = state.with_settings(
            store,
            bound.map(|addr| addr.ip().to_string()).unwrap_or_default(),
            bound.map(|addr| addr.port()).unwrap_or(0),
        );
    }
    let shutdown_restart = restart.clone();
    if let Some(signal) = restart {
        state = state.with_restart(signal);
    }
    let app = create_router_with(state, enable_http, enable_ws, cors_allow_all, json_payload);

    if let Some(addr) = bound {
        tracing::info!("Listening on TCP socket {}", addr);
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        // Ctrl+C and a page-requested restart end the listener the same way;
        // only the log line differs, because a restart continues in a new
        // process rather than ending this one.
        let mut restart_rx = shutdown_restart.map(|signal| signal.0.subscribe());
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Server received shutdown signal, closing active listeners");
            }
            changed = async {
                match restart_rx.as_mut() {
                    Some(rx) => rx.changed().await,
                    // No supervisor: this arm must simply never fire, and a
                    // pending future of the arm's own result type does that.
                    None => std::future::pending::<
                        std::result::Result<(), tokio::sync::watch::error::RecvError>,
                    >()
                    .await,
                }
            } => {
                if changed.is_ok() {
                    tracing::info!("Restart requested from the settings page, closing listeners");
                }
            }
        }
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

    /// A frame that is not valid UTF-8 must cost one dropped packet, not the
    /// process. `panic = "abort"` means an `expect` here would kill the server
    /// mid-match for every connected client at once, with no unwinding.
    #[test]
    fn a_frame_that_is_not_utf8_is_dropped_instead_of_panicking() {
        assert!(ws_text(Bytes::from_static(b"{\"a\":1}")).is_some());
        assert!(ws_text(Bytes::from_static(&[0xff, 0xfe, 0x00])).is_none());
    }

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

    /// `/json` serves the **gosumemory-compatible v1** payload, because that is
    /// what tosu serves there (`packages/server/router/index.ts:43-53`), and
    /// rtosu used to serve v2 on that path instead. A drop-in replacement that
    /// answers a different shape on the same URL is not a drop-in replacement,
    /// and a v1 consumer cannot read v2 at all.
    ///
    /// The three payload routes are pinned together because the mistake this
    /// guards against is silent: a swapped handler still returns 200 and still
    /// returns JSON, so only the shape distinguishes them. `client`,
    /// `settings` and `menu` exist in v1 and not in v2; `profile` and `tourney`
    /// exist in v2 and not in v1.
    #[tokio::test]
    async fn the_json_routes_serve_the_shapes_tosu_serves_on_them() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.state.number = 2;
        sample.play.score = 4652;
        sample.beatmap.id = 2964306;

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, true, true);

        let fetch = |uri: &'static str| {
            let router = app.clone();
            async move {
                let response = router
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{uri}");
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                // The raw bytes, because the order assertion below is about the
                // wire and `serde_json::Value` sorts its keys.
                let text = String::from_utf8(body.to_vec()).expect("utf-8 body");
                let parsed: serde_json::Value = serde_json::from_str(&text).expect("json body");
                (text, parsed)
            }
        };

        // `/json` and `/json/v1` are the same payload.
        let (root_bytes, root) = fetch("/json").await;
        let (v1_bytes, _v1) = fetch("/json/v1").await;
        assert_eq!(root_bytes, v1_bytes, "/json and /json/v1 are aliases");
        assert_eq!(
            crate::testutil::json_key_order(&root_bytes),
            [
                "client",
                "settings",
                "menu",
                "gameplay",
                "resultsScreen",
                "userProfile",
                "tourney"
            ],
            "tosu's v1 top level, in wire order (router/index.ts:43-53)"
        );
        for v1_only in ["settings", "menu", "gameplay", "userProfile"] {
            assert!(root.get(v1_only).is_some(), "v1 carries {v1_only}");
        }

        // `/json/v2` is the v2 shape. `profile` is v2-only and `gameplay` is
        // v1-only, so the two routes are distinguishable by a single key either
        // way -- `tourney` is in **both** shapes and so distinguishes nothing.
        let (_v2_bytes, v2) = fetch("/json/v2").await;
        assert!(
            v2.get("profile").is_some() && root.get("profile").is_none(),
            "profile is the v2-only key"
        );
        for v1_only in ["settings", "menu", "gameplay"] {
            assert!(
                v2.get(v1_only).is_none(),
                "v2 has no {v1_only}; settings is the accepted divergence"
            );
        }
    }

    /// `server.json_payload` chooses what `/json` serves, and `/json/v1` and
    /// `/json/v2` are unaffected by it.
    ///
    /// Repointing `/json` to v1 was a breaking change, so the escape hatch has to
    /// be real: an operator whose overlay reads v2 from that path sets
    /// `json_payload = "v2"` and is back where they were, without waiting for
    /// every consumer to move to `/json/v2`.
    #[tokio::test]
    async fn the_json_payload_option_puts_the_old_shape_back_on_json() {
        // The config value parses to the enum, and an unknown value falls back to
        // v1 rather than to whatever the operator was trying to avoid.
        assert_eq!(JsonPayload::from_config("v1"), JsonPayload::V1);
        assert_eq!(JsonPayload::from_config("V2"), JsonPayload::V2);
        assert_eq!(JsonPayload::from_config(" v1 "), JsonPayload::V1);
        assert_eq!(JsonPayload::from_config(""), JsonPayload::V1);
        assert_eq!(JsonPayload::from_config("v3"), JsonPayload::V1);
        assert_eq!(JsonPayload::from_config("yes"), JsonPayload::V1);

        let shape_of = |router: Router, uri: &'static str| async move {
            let body = axum::body::to_bytes(
                router
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap()
                    .into_body(),
                usize::MAX,
            )
            .await
            .unwrap();
            serde_json::from_slice::<serde_json::Value>(&body).expect("json body")
        };

        for (payload, expect_v1) in [(JsonPayload::V1, true), (JsonPayload::V2, false)] {
            let mut sample = TosuV2Packet::default();
            sample.client = "stable".to_string();
            let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));

            let app = create_router_with(AppState::new(rx.clone()), true, true, true, payload);
            let root = shape_of(app, "/json").await;
            // `settings` is the v1-only marker, and `profile` the v2-only one.
            assert_eq!(
                root.get("settings").is_some(),
                expect_v1,
                "/json with {payload:?}"
            );
            assert_eq!(
                root.get("profile").is_some(),
                !expect_v1,
                "/json with {payload:?}"
            );

            // The explicit paths never move, whatever the option says.
            let app = create_router_with(AppState::new(rx.clone()), true, true, true, payload);
            assert!(
                shape_of(app, "/json/v1").await.get("settings").is_some(),
                "/json/v1 is v1 under {payload:?}"
            );
            let app = create_router_with(AppState::new(rx), true, true, true, payload);
            assert!(
                shape_of(app, "/json/v2").await.get("profile").is_some(),
                "/json/v2 is v2 under {payload:?}"
            );
        }
    }

    /// The three data sockets send **no frame at all** while no osu! is
    /// attached, and that includes `/websocket/v2`.
    ///
    /// This one stream used to substitute an empty `Bytes` for the detached case
    /// instead of skipping the send. Empty bytes are valid UTF-8, so `ws_text`
    /// turned them into a zero-length `Message::Text("")` and transmitted it --
    /// which is not silence, it is a frame that every consumer of this socket
    /// feeds to `JSON.parse`. Both bundled overlays do exactly that, so the
    /// game's exit produced a `SyntaxError` in each of them at the
    /// between-maps restart.
    ///
    /// The assertion is on the *shape* of the frame, not just its absence: a zero
    /// length text frame is a distinct value from `None`, and only `None` means
    /// nothing was sent.
    #[tokio::test]
    async fn the_data_sockets_send_nothing_while_nothing_is_attached() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize").detached());
        let mut rx = rx;

        // None, and specifically not a text frame of length 0.
        assert!(
            v2_frame(&mut rx).is_none(),
            "/websocket/v2 must send nothing, not an empty frame"
        );
        assert!(v1_frame(&mut rx).is_none(), "/ws");
        assert!(precise_frame(&mut rx).is_none(), "/websocket/v2/precise");

        // Once something is attached, all three produce a frame again -- the
        // guard is not "never send".
        let attached = PublishedPacket::new(TosuV2Packet::default()).expect("serialize");
        let (_tx2, mut rx2) = watch::channel(attached);
        for (name, frame) in [
            ("/websocket/v2", v2_frame(&mut rx2)),
            ("/ws", v1_frame(&mut rx2)),
            ("/websocket/v2/precise", precise_frame(&mut rx2)),
        ] {
            let frame = frame.unwrap_or_else(|| panic!("{name} must send when attached"));
            let Message::Text(text) = frame else {
                panic!("{name} must send a text frame");
            };
            assert!(
                !text.is_empty(),
                "{name} sent a zero-length frame even while attached"
            );
        }
    }

    /// With no osu! attached, **all four** `/json*` routes answer    /// `500 {"error":"osu is not ready/running"}` -- and none of them serves a
    /// payload.
    ///
    /// tosu's guard is identical on all four (`router/index.ts:44-46`,
    /// `router/v2.ts:9-15` and `:17-23`, `router/scApi.ts:6-12`) and the thrown
    /// error becomes `500` plus `sendJson(res, {error: message})`
    /// (`utils/http.ts:186-207`), so all four answers are byte-identical. They
    /// used to be four `200`s with different bodies, and a payload containing
    /// `client: "none"` and `state.name: "notRunning"` -- neither of which is a
    /// value tosu can emit -- which forced a consumer to recognise rtosu's
    /// invention instead of reading tosu's error.
    ///
    /// The four are asserted together on purpose: they are one contract, and
    /// leaving one of them as a `200` is exactly the state this replaces.
    #[tokio::test]
    async fn every_json_route_answers_tosus_not_ready_when_nothing_is_attached() {
        // A packet that looks like a real, running client. If any route leaks it
        // while detached, the body assertions below catch it even though the
        // status check would already have failed.
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.state.number = 2;
        sample.state.name = "play".to_string();
        sample.play.score = 1_234_567;

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize").detached());
        let app = create_router(AppState::new(rx), true, true, true);

        for uri in [
            "/json",
            "/json/v1",
            "/json/v2",
            "/json/v2/precise",
            "/json/sc",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{uri} must refuse rather than serve the last packet"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/json"),
                "{uri} content type"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let text = String::from_utf8(body.to_vec()).expect("utf-8 body");
            assert_eq!(
                text, r#"{"error":"osu is not ready/running"}"#,
                "{uri} body must be tosu's, verbatim"
            );
        }

        // And the same routes serve normally once something is attached, so the
        // guard is not just "always refuse".
        let attached = PublishedPacket::new({
            let mut packet = TosuV2Packet::default();
            packet.client = "stable".to_string();
            packet.state.number = 2;
            packet.state.name = "play".to_string();
            packet
        })
        .expect("serialize");
        assert!(attached.attached);
        let (_tx2, rx2) = watch::channel(attached);
        let app = create_router(AppState::new(rx2), true, true, true);
        for uri in [
            "/json",
            "/json/v1",
            "/json/v2",
            "/json/v2/precise",
            "/json/sc",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri} when attached");
        }
    }

    /// `/json/v2/precise` serves `{keys, hitErrors, tourney}` -- three keys, and
    /// none of the v2 payload. It used to be the same handler as `/json/v2`, so
    /// the endpoint whose entire purpose is a cheap high-frequency feed returned
    /// the whole packet including the strain graph, and returned neither `keys`
    /// nor `hitErrors`. Measured live against tosu 4.26.2 on the same map in the
    /// same state: 221 bytes there, 24,039 here.
    ///
    /// The size assertion is the point. A shape assertion alone would pass on a
    /// payload that carried the graph in a fourth key, and the byte count is what
    /// an overlay's bandwidth actually depends on.
    #[tokio::test]
    async fn the_precise_route_serves_three_keys_and_not_the_v2_packet() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.state.number = 2;
        sample.beatmap.id = 2964306;
        sample.play.key_overlay.k1 = crate::v2::KeyOverlayButton {
            is_pressed: true,
            count: 11,
        };
        sample.play.hit_error_array = std::sync::Arc::from(vec![-3i16, 0, 7, 12, -20]);
        sample.performance.graph = crate::v2::PrecomputedGraph::new(&crate::v2::PerformanceGraph {
            series: (0..5)
                .map(|index| crate::v2::GraphSeries {
                    name: format!("series{index}"),
                    data: (0..4_778).map(|point| point as f64 * 0.1234).collect(),
                })
                .collect(),
            xaxis: (0..4_778).map(|point| point as f64 * 400.0).collect(),
        });

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, true, true);

        let body = |app: Router, uri: &'static str| async move {
            let response = app
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
        };

        let precise = body(app.clone(), "/json/v2/precise").await;
        let full = body(app, "/json/v2").await;

        assert!(
            precise.len() < 5_000 && full.len() > 100_000,
            "precise {} bytes against v2 {} bytes: the precise route must not \
             carry the graph",
            precise.len(),
            full.len()
        );

        let text = String::from_utf8(precise.to_vec()).expect("utf-8 body");
        assert_eq!(
            crate::testutil::json_key_order(&text),
            ["keys", "hitErrors", "tourney"],
            "tosu's three keys, in wire order (buildResultV2Precise.ts:68-87)"
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json body");
        assert_eq!(
            parsed["keys"]["k1"]["count"], 11,
            "the read reaches the wire"
        );
        assert_eq!(
            parsed["keys"]["k1"]["isPressed"],
            serde_json::json!(true),
            "and the pressed flag, which is a bool on the wire"
        );
        assert_eq!(
            parsed["hitErrors"].as_array().map(Vec::len),
            Some(5),
            "the hit errors are the packet's own list"
        );
        assert!(parsed["tourney"].is_array());
    }

    /// `/json/sc` serves the StreamCompanion payload: 136 flat keys, none of
    /// which appear in v1 or v2. Before this route existed the path 404'd, and
    /// an SC client pointed at rtosu had nothing to read at all.
    #[tokio::test]
    async fn test_http_json_sc_endpoint() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.state.number = 2;
        sample.play.score = 4652;
        sample.beatmap.id = 2964306;

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, true, true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/json/sc")
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
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "tosu sends application/json with no charset (utils/index.ts:81)"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Flat and 136 keys, with no subtrees.
        let object = parsed.as_object().expect("an object");
        assert_eq!(object.len(), 136, "SC key count");
        assert!(!parsed.get("menu").is_some());
        assert!(!parsed.get("beatmap").is_some());
        assert!(!parsed.get("play").is_some());

        // Spot-check leaves across the payload, including the two shapes most
        // likely to be built wrongly.
        assert_eq!(parsed["osuIsRunning"], 1);
        assert_eq!(parsed["score"], 4652);
        assert_eq!(parsed["mapid"], 2964306);
        assert_eq!(parsed["dl"], "https://osu.ppy.sh/b/2964306");
        assert_eq!(parsed["status"], 2, "play maps to SC's Playing");
        assert_eq!(parsed["rawStatus"], 2);
        assert!(parsed["keyOverlay"].is_string(), "JSON in a string");
        assert!(parsed["mapStrains"].is_object());
        assert!(parsed["mapKiaiPoints"].is_array());
        assert!(parsed["mapKiaiPoints"].as_array().unwrap().is_empty());
    }

    /// The route is gated by `enable_http` like every other `/json*` route, so an
    /// HTTP-disabled server must not answer it. `/json/sc` is not a websocket
    /// route, which is asserted by the ws-disabled half below.
    #[tokio::test]
    async fn test_json_sc_follows_the_http_enable_flag() {
        let sample = TosuV2Packet::default();

        let (_tx1, rx1) = watch::channel(PublishedPacket::new(sample.clone()).expect("serialize"));
        let enabled = create_router(AppState::new(rx1), true, true, true);
        let ok = enabled
            .oneshot(
                Request::builder()
                    .uri("/json/sc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        let (_tx2, rx2) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let disabled = create_router(AppState::new(rx2), false, true, true);
        let refused = disabled
            .oneshot(
                Request::builder()
                    .uri("/json/sc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);

        // WS-disabled must still serve it: it is a plain GET.
        let (_tx3, rx3) = watch::channel(PublishedPacket::new(TosuV2Packet::default()).expect("s"));
        let no_ws = create_router(AppState::new(rx3), true, false, true);
        let served = no_ws
            .oneshot(
                Request::builder()
                    .uri("/json/sc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(served.status(), StatusCode::OK);
    }

    /// `/tokens` is the one socket that reads from the client. Before this it
    /// 404'd, so an SC client had no way to ask for a subset of the payload.
    #[tokio::test]
    async fn test_tokens_route_upgrades_and_answers_the_requested_leaves() {
        use crate::ws_filters::{filter_json, parse_filters, parse_packet};

        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();
        sample.play.score = 4652;
        sample.beatmap.id = 2964306;

        let json = serde_json::to_vec(&sample).expect("serialize");
        // A dotted filter is a single key, not a path -- so the request uses the
        // nested spelling to actually narrow. `ws_filters` pins the dotted case.
        let filters = parse_filters(
            r#"[{"field":"play","keys":["score"]},{"field":"beatmap","keys":["id"]}]"#,
        )
        .expect("filters");
        let bytes = filter_json(&filters, &parse_packet(&json).expect("parse")).expect("frame");
        let frame: serde_json::Value = serde_json::from_slice(&bytes).expect("frame parses");
        // Only the two requested leaves, in filter order.
        assert_eq!(
            frame,
            serde_json::json!({"play": {"score": 4652}, "beatmap": {"id": 2964306}})
        );
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"play":{"score":4652},"beatmap":{"id":2964306}}"#
        );

        // The route must be wired to a WebSocket handler, not merely exist.
        // axum's `WebSocketUpgrade` extractor rejects a plain GET with `400`,
        // which is the assertion that distinguishes "the route is a WS upgrade"
        // from "the route is missing" (`404`) and from "the route answers a plain
        // GET with a payload" (`200`). A real handshake -- filters applied and
        // filtered frames received -- is verified end to end by
        // `validations/tokens_probe.py`.
        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, true, true);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tokens")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "the /tokens route must exist"
        );
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "the /tokens route must require a websocket upgrade"
        );
    }

    /// A rejected or non-command message must leave the connection's filters
    /// alone, which is tosu's behaviour (`commands.ts:161` assigns only on
    /// success). Driving `handle_token_message` directly keeps that out of a real
    /// socket, and covers the length cap that only a live socket could reach.
    #[test]
    fn test_a_token_message_only_replaces_the_filters_on_success() {
        use crate::ws_filters::Filter;

        let mut filters = vec![Filter::Key("play.score".to_string())];

        // A bare array on /tokens is normalised and applied.
        assert!(handle_token_message(
            &Message::text(r#"["beatmap.id"]"#),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(filters, vec![Filter::Key("beatmap.id".to_string())]);

        // A bare object array contains colons, so tosu's `data.includes(':')` check
        // passes it through without the `applyFilters:` prefix and it is not a
        // command at all. Verified live against tosu: the previous filter stays
        // in place. Reproduced deliberately, so the test asserts the refusal.
        assert!(handle_token_message(
            &Message::text(r#"[{"field":"play","keys":["score"]}]"#),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(
            filters,
            vec![Filter::Key("beatmap.id".to_string())],
            "a bare object array is not a command"
        );

        // Sent with the explicit prefix, the same array is applied.
        assert!(handle_token_message(
            &Message::text(r#"applyFilters:[{"field":"play","keys":["score"]}]"#),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(
            filters,
            vec![Filter::Nested {
                field: "play".to_string(),
                keys: vec![Filter::Key("score".to_string())],
            }]
        );

        // From here on the live filter is the nested one, so that is what a
        // rejected message has to leave in place.
        let applied = || {
            vec![Filter::Nested {
                field: "play".to_string(),
                keys: vec![Filter::Key("score".to_string())],
            }]
        };

        // A malformed payload leaves the previous list in place.
        assert!(handle_token_message(
            &Message::text("applyFilters:{not json"),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(filters, applied());

        // A non-array payload is rejected the same way.
        assert!(handle_token_message(
            &Message::text(r#"applyFilters:{"a":1}"#),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(filters, applied());

        // A message that is not a command is ignored, and the connection stays up.
        assert!(handle_token_message(
            &Message::text("hello"),
            "/tokens",
            &mut filters,
        ));
        assert_eq!(filters, applied());

        // A bare array whose entries parse to nothing empties the list, which
        // re-enables the full payload -- matching tosu's `filters.length > 0` gate.
        // It parses, so it is assigned; it is the *content* that is empty.
        assert!(handle_token_message(
            &Message::text("[1,2,3]"),
            "/tokens",
            &mut filters
        ));
        assert!(filters.is_empty());

        // An oversized message is dropped without being parsed, and the
        // connection survives it.
        let huge = format!(r#"applyFilters:["{}"]"#, "a".repeat(70 * 1024));
        assert!(handle_token_message(
            &Message::text(huge),
            "/tokens",
            &mut filters
        ));

        // A close frame ends the connection.
        assert!(!handle_token_message(
            &Message::Close(None),
            "/tokens",
            &mut filters,
        ));
    }

    /// A payload that cannot be filtered must not stop the stream: the caller
    /// falls back to the full payload for that tick.
    #[test]
    fn test_filter_frame_returns_none_only_when_filtering_fails() {
        use crate::ws_filters::{filter_json, parse_filters};

        let filters = parse_filters(r#"[{"field":"play","keys":["score"]}]"#).expect("filters");
        let good = br#"{"play":{"score":4652,"accuracy":68.75}}"#;
        assert!(
            filter_frame(&filters, good).is_some(),
            "a valid payload must produce a frame"
        );
        assert!(
            filter_frame(&filters, b"{not json").is_none(),
            "an unparseable payload must be reported as a filtering failure"
        );
        // An empty filter list is the unfiltered path, so it is not a frame here.
        assert!(filter_frame(&[], good).is_none());
        // And the filtered frame carries only the requested leaf.
        let bytes = filter_json(&filters, &serde_json::json!({"play":{"score":1},"x":2})).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), r#"{"play":{"score":1}}"#);
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
        let res = start_server(
            "127.0.0.1",
            0,
            false,
            false,
            true,
            None,
            rx,
            JsonPayload::V1,
            None,
            None,
        )
        .await;
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
        // The served URL carries the directory name percent-encoded, because a
        // raw space is not legal in a URL and an unencoded `%` would be read
        // back as an escape.
        assert!(body.contains("/overlays/Team%20Bar/"));

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
        // '/ws' is an identity mapping, not a redirect onto the v2 socket: rtosu
        // serves the real gosumemory payload there, and a v1 overlay cannot read
        // v2. This string was pinned in three places -- here, in the shim's own
        // substring test, and in the route table it duplicated -- which is how the
        // mapping survived every other change. See `K-02` in validations/audits/audit-1.0.5.md.
        assert!(body.contains("'/ws': '/ws'"));
        assert!(!body.contains("'/ws': '/websocket/v2'"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The song and skin routes have to serve bytes, not merely exist.
    ///
    /// tosu serves the whole Songs tree at `/Songs/*` and `/files/beatmap/*`
    /// (`router/v1.ts:5`, `router/v2.ts:39-59`) and skins at `/files/skin/*`
    /// (`router/v2.ts:61-96`), and the overlay shim rewrites an overlay's
    /// `/Songs/` onto `/files/beatmap/`. Both destinations used to 404, which is
    /// why a drop-in overlay could show nothing but the current map's metadata.
    #[tokio::test]
    async fn the_song_and_skin_routes_serve_the_file_that_was_asked_for() {
        let root = temp_overlay_root("songfiles");
        let songs = root.join("songs");
        let skins = root.join("skins");
        let mapset = songs.join("123 Artist - Title");
        std::fs::create_dir_all(&mapset).unwrap();
        std::fs::create_dir_all(&skins).unwrap();

        // Names chosen to be awkward in a path but legal on disk.
        let audio = mapset.join("audio.mp3");
        let beatmap_file = mapset.join("map.osu");
        let element = skins.join("rank.png");
        std::fs::write(&audio, b"ID3-bytes").unwrap();
        std::fs::write(&beatmap_file, b"[Beatmap]\n").unwrap();
        std::fs::write(&element, b"PNG-bytes").unwrap();

        let mut sample = TosuV2Packet::default();
        sample.folders.songs = songs.to_string_lossy().into_owned();
        sample.folders.skin = skins.to_string_lossy().into_owned();
        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        for (uri, expected) in [
            ("/Songs/123%20Artist%20-%20Title/audio.mp3", "ID3-bytes"),
            (
                "/files/beatmap/123%20Artist%20-%20Title/audio.mp3",
                "ID3-bytes",
            ),
            (
                "/files/beatmap/123%20Artist%20-%20Title/map.osu",
                "[Beatmap]\n",
            ),
            ("/files/skin/rank.png", "PNG-bytes"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri} should be served");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(bytes.as_ref(), expected.as_bytes(), "{uri} body");
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Containment is the part that has to hold: these are new file-serving
    /// endpoints, and tosu's own `directoryWalker` has no traversal protection at
    /// all (`utils/directories.ts:56` is a bare `path.join` of a post-decode,
    /// caller-supplied path). rtosu's background route already refuses to serve
    /// outside the songs folder, and these routes must not be the way around it.
    #[tokio::test]
    async fn the_song_and_skin_routes_cannot_escape_their_folders() {
        let root = temp_overlay_root("songescape");
        let songs = root.join("songs");
        let skins = root.join("skins");
        std::fs::create_dir_all(&songs).unwrap();
        std::fs::create_dir_all(&skins).unwrap();
        let secret = root.join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();

        let mut sample = TosuV2Packet::default();
        sample.folders.songs = songs.to_string_lossy().into_owned();
        sample.folders.skin = skins.to_string_lossy().into_owned();
        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        // A relative climb, an encoded climb, a backslash climb, an absolute
        // path, and the same through the skin route.
        for uri in [
            "/Songs/../secret.txt",
            "/Songs/%2e%2e/secret.txt",
            "/Songs/..%2fsecret.txt",
            "/Songs/..\\secret.txt",
            "/files/beatmap/../../secret.txt",
            "/files/beatmap/..%2f..%2fsecret.txt",
            "/files/skin/../secret.txt",
            "/files/skin/%2e%2e/secret.txt",
        ] {
            let (status, body) = get_text(app.clone(), uri).await;
            assert_ne!(
                body.trim(),
                "top secret",
                "{uri} served a file from outside the songs folder"
            );
            assert!(
                status == StatusCode::NOT_FOUND || body.trim().is_empty(),
                "{uri} should be a 404, got {status} with {body:?}"
            );
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Close the loop between the shim and the router.
    ///
    /// `overlays::shim_source_rewrites_the_documented_tosu_routes` asserts the shim
    /// *contains* the right characters, which cannot notice that a route it
    /// advertises was never registered. Deleting `/files/beatmap/background` from
    /// `create_router` would leave that test green while every overlay asking for
    /// the beatmap background got a 404. This test issues real requests instead.
    ///
    /// A request cannot simply be required to be non-404, because a *registered*
    /// route may legitimately 404: `/files/beatmap/background` answers 404 when no
    /// beatmap is loaded, which is the state this runs in. axum's unmatched-path
    /// fallback is distinguishable by its shape -- 404 with an empty body -- so the
    /// discriminator is the same null control `validations/compare_routes.py` uses
    /// against a live server.
    ///
    /// Fails today: the shim advertises `/Songs/` and `/files/skin/`, rewritten onto
    /// paths with no handler. See `K-01` and `K-02` in validations/audits/audit-1.0.5.md.
    #[tokio::test]
    async fn every_file_route_the_shim_advertises_is_registered() {
        // Straight off the one const the shim itself is generated from, so this
        // cannot fall out of step with what is actually served.
        let destinations: Vec<String> = crate::overlays::FILE_ROUTES
            .iter()
            .map(|(_, to)| (*to).to_string())
            .collect();
        assert!(
            destinations.len() >= 4,
            "FILE_ROUTES should advertise at least four routes, got {destinations:?}"
        );

        // And the generated shim really does contain each of them.
        let js = crate::overlays::overlay_shim_js();
        for (from, to) in crate::overlays::FILE_ROUTES {
            let entry = format!("['{from}', '{to}']");
            assert!(js.contains(&entry), "shim is missing {entry}");
        }

        let root = temp_overlay_root("shimroutes");
        let mut unregistered: Vec<String> = Vec::new();
        for destination in &destinations {
            // A destination ending in '/' is a prefix the shim appends the rest of
            // the overlay's path to, so probe below it. Anything else is an exact
            // path, and must be probed as-is: appending to '/files/beatmap/background'
            // would ask for '/files/beatmap/backgroundprobe', which is not the
            // registered route and would report a false positive.
            let probe = if destination.ends_with('/') {
                format!("{destination}probe")
            } else {
                destination.clone()
            };
            let (status, body) = get_text(overlay_app(root.clone()), &probe).await;
            if status == StatusCode::NOT_FOUND && body.is_empty() {
                unregistered.push(destination.clone());
            }
        }

        let _ = std::fs::remove_dir_all(&root);
        assert!(
            unregistered.is_empty(),
            "the shim rewrites overlays onto paths the router does not register, so every \
             overlay using them gets a 404: {unregistered:?}"
        );
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
        // The served file has to live inside the songs folder: containment is
        // measured against it, so an image outside the root is now a 404.
        let songs = root.join("songs");
        std::fs::create_dir_all(&songs).unwrap();
        let image = songs.join("bg.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xD9]).expect("write image");

        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 77;
        sample.folders.songs = songs.to_string_lossy().into_owned();
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

    /// Build a router whose packet points at `songs` with the given memory-read
    /// values, so the containment tests differ only in what osu! claimed.
    fn background_app(songs: &std::path::Path, background: &str, relative: &str) -> Router {
        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 1;
        sample.folders.songs = songs.to_string_lossy().into_owned();
        sample.files.background = background.to_string();
        sample.direct_path.beatmap_background = relative.to_string();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        create_router(AppState::new(rx), true, false, true)
    }

    #[tokio::test]
    async fn test_beatmap_background_outside_the_songs_folder_is_not_found() {
        let root = temp_overlay_root("background-outside");
        let songs = root.join("songs");
        std::fs::create_dir_all(&songs).unwrap();
        // A real file, but nowhere near the songs folder.
        let outside = root.join("private.txt");
        std::fs::write(&outside, b"top secret").unwrap();

        let app = background_app(&songs, outside.to_str().unwrap(), "");
        let (status, body) = get_text(app, "/files/beatmap/background").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("top secret"), "file outside songs leaked");
    }

    #[tokio::test]
    async fn test_beatmap_background_traversal_out_of_songs_is_not_found() {
        let root = temp_overlay_root("background-traversal");
        let songs = root.join("songs");
        std::fs::create_dir_all(&songs).unwrap();
        std::fs::write(root.join("private.txt"), b"top secret").unwrap();

        // `direct_path.beatmap_background` is a raw memory read joined with `\`,
        // so `..` in either component used to escape the songs folder.
        let app = background_app(&songs, "", "..\\..\\private.txt");
        let (status, body) = get_text(app, "/files/beatmap/background").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("top secret"), "traversal leaked a file");
    }

    #[tokio::test]
    async fn test_beatmap_background_traversal_back_into_songs_is_served() {
        let root = temp_overlay_root("background-reenter");
        let songs = root.join("songs");
        let dir = songs.join("123 Artist - Song");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bg.jpg"), [7, 8, 9]).unwrap();

        // The `..` climbs out and comes back, ending inside the songs folder.
        // Canonicalization is the rule, so this is a normal file and is served.
        let app = background_app(&songs, "", "outside\\..\\123 Artist - Song\\bg.jpg");
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
        assert_eq!(bytes.as_ref(), &[7, 8, 9]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_beatmap_background_without_a_songs_folder_is_not_found() {
        let root = temp_overlay_root("background-no-songs");
        let image = root.join("bg.jpg");
        std::fs::write(&image, [1, 2, 3]).unwrap();

        let mut sample = TosuV2Packet::default();
        sample.beatmap.set = 77;
        // Deliberate fail-closed behaviour: without a songs root there is
        // nothing to be contained by, so an otherwise valid file is refused.
        sample.folders.songs = String::new();
        sample.files.background = image.to_string_lossy().into_owned();

        let (_tx, rx) = watch::channel(PublishedPacket::new(sample).expect("serialize"));
        let app = create_router(AppState::new(rx), true, false, true);

        let (status, _) = get_text(app, "/files/beatmap/background").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_folder_url_redirects_with_an_encoded_slug() {
        let root = temp_overlay_root("redirect-encoded");
        write_overlay_file(&root, "Team Bar/index.html", "<html></html>");

        let response = overlay_app(root.clone())
            .oneshot(
                Request::builder()
                    .uri("/overlays/Team%20Bar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/overlays/Team%20Bar/"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn test_overlay_folder_with_a_percent_sign_resolves_when_encoded() {
        let root = temp_overlay_root("percent-slug");
        write_overlay_file(&root, "100% Pure/index.html", "<html>pure</html>");

        // The dashboard advertises the encoded form, and that is the form that
        // has to resolve back to the directory named `100% Pure`, for the entry
        // document and for an asset inside it.
        let app = overlay_app(root.clone());
        let (status, body) = get_text(app.clone(), "/overlays/100%25%20Pure/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("pure"));

        let (status, body) = get_text(app, "/overlays/100%25%20Pure/index.html").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("pure"));

        std::fs::remove_dir_all(&root).unwrap();
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

    // ---- the landing page and its API ----

    use crate::config::AppConfig;

    fn loopback() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 51_234))
    }

    /// A settings store over a path in a fresh temp directory.
    fn settings_store(tag: &str) -> Arc<SettingsStore> {
        let dir = std::env::temp_dir().join(format!("rtosu-server-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the settings directory");
        Arc::new(
            SettingsStore::new(dir.join("config.toml"), AppConfig::default())
                .expect("the default config builds a store"),
        )
    }

    /// A router carrying a settings store and a peer address, which is what the
    /// settings routes need to make a decision.
    fn settings_app(store: Arc<SettingsStore>, peer: SocketAddr) -> Router {
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        create_router(
            AppState::new(rx).with_settings(store, "127.0.0.1", 24050),
            true,
            true,
            true,
        )
        .layer(MockConnectInfo(peer))
    }

    /// A `POST /api/settings` as a client would send it.
    ///
    /// The peer comes from the router's own [`MockConnectInfo`] layer rather than
    /// from here: two such layers would both write the extension and the inner
    /// one would win, which is exactly the bug this signature avoids.
    async fn post_settings(
        app: Router,
        content_type: Option<&str>,
        origin: Option<&str>,
        body: &str,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method("POST").uri("/api/settings");
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = app
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `GET /` answers with a page rather than the 404 it used to fall through
    /// to, and it works with nothing attached: no store, no overlay directory.
    #[tokio::test]
    async fn the_root_serves_the_landing_page() {
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let (status, body) =
            get_text(create_router(AppState::new(rx), true, false, true), "/").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<!DOCTYPE html>"));
        assert!(
            body.contains("Settings are unavailable"),
            "a page without a store has to say so"
        );

        // And the API says 404 rather than pretending it has settings.
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let (status, _) = get_text(
            create_router(AppState::new(rx), true, false, true),
            "/api/settings",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The zero-port bypass still means "no page either", because the route
        // lives inside the HTTP block.
        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let (status, _) = get_text(create_router(AppState::new(rx), false, false, true), "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `POST /api/restart`: the CLI-wired supervisor gets a signalled restart,
    /// and every other situation is refused rather than quietly dropped.
    #[tokio::test]
    async fn restart_needs_a_supervisor_and_a_loopback_request() {
        async fn post_restart(app: Router, origin: Option<&str>) -> (StatusCode, String) {
            let mut request = Request::builder().method("POST").uri("/api/restart");
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            let response = app
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }

        // No supervisor wired: 409, because a request nobody honours must say so.
        let store = settings_store("restart-none");
        let (status, body) = post_restart(settings_app(store, loopback()), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("supervisor"), "{body}");

        // Supervisor wired, loopback peer: accepted, and the signal fires after
        // the grace period that lets the response flush.
        let store = settings_store("restart-ok");
        let (signal, mut restart_rx) = RestartSignal::channel();
        let (_tx, packet_rx) = watch::channel(PublishedPacket::default_packet());
        let app = create_router(
            AppState::new(packet_rx)
                .with_settings(store.clone(), "127.0.0.1", 24050)
                .with_restart(signal),
            true,
            false,
            true,
        )
        .layer(MockConnectInfo(loopback()));
        let (status, body) = post_restart(app, Some("http://127.0.0.1:24050")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"restarting\":true"), "{body}");
        tokio::time::timeout(std::time::Duration::from_secs(3), restart_rx.changed())
            .await
            .expect("the restart signal fires")
            .expect("the sender outlives the request");
        assert!(*restart_rx.borrow_and_update());

        // A LAN peer is refused even though the store itself would allow the
        // read: restarting kills every live overlay socket, so the guard is
        // unconditional.
        let (status, body) = post_restart(
            {
                let (_tx, packet_rx) = watch::channel(PublishedPacket::default_packet());
                create_router(
                    AppState::new(packet_rx)
                        .with_settings(store.clone(), "127.0.0.1", 24050)
                        .with_restart(RestartSignal::channel().0),
                    true,
                    false,
                    true,
                )
                .layer(MockConnectInfo(SocketAddr::from((
                    [192, 168, 1, 10],
                    51_234,
                ))))
            },
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        // A foreign Origin from a loopback peer is refused too -- the same CSRF
        // shape the settings write guard refuses.
        let (status, body) = post_restart(
            {
                let (_tx, packet_rx) = watch::channel(PublishedPacket::default_packet());
                create_router(
                    AppState::new(packet_rx)
                        .with_settings(store, "127.0.0.1", 24050)
                        .with_restart(RestartSignal::channel().0),
                    true,
                    false,
                    true,
                )
                .layer(MockConnectInfo(loopback()))
            },
            Some("http://evil.example"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    /// `GET /api/settings` carries the config, the field table, and whether this
    /// viewer may write.
    #[tokio::test]
    async fn settings_get_returns_the_config_the_fields_and_the_write_flag() {
        let store = settings_store("get");
        let (status, body) =
            get_text(settings_app(store.clone(), loopback()), "/api/settings").await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["writable"], serde_json::json!(true));
        assert_eq!(parsed["writable_reason"], serde_json::Value::Null);
        assert_eq!(
            parsed["config"]["poll"]["poll_rate_hz"],
            serde_json::json!(60)
        );
        assert_eq!(
            parsed["config"]["server"]["settings_write_local_only"],
            serde_json::json!(true)
        );
        assert!(
            parsed["fields"]
                .as_array()
                .expect("fields are an array")
                .iter()
                .any(|field| field["key"] == "poll.poll_rate_hz"),
            "the field table must reach the page"
        );
        let config: AppConfig =
            serde_json::from_value(parsed["config"].clone()).expect("the config round-trips");
        assert_eq!(config.server.port, 24050);

        // A viewer on the LAN sees everything and is told why it cannot write.
        let (status, body) = get_text(
            settings_app(store, SocketAddr::from(([192, 168, 1, 10], 51_234))),
            "/api/settings",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["writable"], serde_json::json!(false));
        assert_eq!(
            parsed["writable_reason"],
            serde_json::json!("settings_write_local_only")
        );
    }

    /// The happy path: a local peer saves, the file is rewritten with its
    /// comments intact, and the poll loop's live view moves.
    #[tokio::test]
    async fn settings_post_from_localhost_writes_the_file_and_publishes_the_change() {
        let store = settings_store("post");
        let mut live_rx = store.subscribe();

        let (status, body) = post_settings(
            settings_app(store.clone(), loopback()),
            Some("application/json; charset=utf-8"),
            Some("http://localhost:24050"),
            r#"{ "poll.poll_rate_hz": 120, "features.enable_pp": false, "logging.level": "debug" }"#,
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["ok"], serde_json::json!(true));
        assert_eq!(parsed["applied"].as_array().expect("applied").len(), 3);
        assert_eq!(
            parsed["restart_required"],
            serde_json::json!(["logging.level"]),
            "only the log level needs a restart"
        );
        assert_eq!(
            parsed["config"]["poll"]["poll_rate_hz"],
            serde_json::json!(120)
        );

        // The file on disk carries the change and kept the template's comments.
        let text = std::fs::read_to_string(store.path()).expect("the config must be written");
        assert!(text.contains("poll_rate_hz = 120"));
        assert!(text.contains("enable_pp = false"));
        assert!(text.contains("# A high-performance native Rust memory reader"));
        let written: AppConfig = toml::from_str(&text).expect("the written file parses");
        written.validate().expect("the written file validates");
        assert_eq!(written.logging.level, "debug");

        // And the poll loop can see it on its next tick.
        assert!(live_rx.has_changed().unwrap_or(false));
        let live = live_rx.borrow_and_update().clone();
        assert_eq!(live.poll_interval, std::time::Duration::from_millis(8));
        assert!(!live.enable_pp);
        assert_eq!(store.config().poll.poll_rate_hz, 120);
    }

    /// The write guard: a LAN viewer, and a browser on this machine that was
    /// loaded from somewhere else, are both refused.
    #[tokio::test]
    async fn settings_post_is_refused_for_a_lan_peer_or_a_foreign_origin() {
        let store = settings_store("guard");

        let (status, body) = post_settings(
            settings_app(store.clone(), SocketAddr::from(([192, 168, 1, 10], 51_234))),
            Some("application/json"),
            None,
            r#"{ "poll.poll_rate_hz": 120 }"#,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("restricted to localhost"));

        let (status, body) = post_settings(
            settings_app(store.clone(), loopback()),
            Some("application/json"),
            Some("http://evil.example"),
            r#"{ "poll.poll_rate_hz": 120 }"#,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("restricted to localhost"));

        // Neither refusal may have written or applied anything.
        assert!(!store.path().exists());
        assert_eq!(store.config().poll.poll_rate_hz, 60);
    }

    /// A body that is not declared as JSON, and one that is too large, are
    /// refused before they are parsed.
    #[tokio::test]
    async fn settings_post_refuses_a_wrong_content_type_and_an_oversized_body() {
        let store = settings_store("media");

        let (status, body) = post_settings(
            settings_app(store.clone(), loopback()),
            Some("text/plain"),
            None,
            r#"{ "poll.poll_rate_hz": 120 }"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(body.contains("application/json"));

        let (status, _) = post_settings(
            settings_app(store.clone(), loopback()),
            None,
            None,
            r#"{ "poll.poll_rate_hz": 120 }"#,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a missing content type is not a guess"
        );

        let huge = format!(
            "{{\"logging.level\": \"{}\"}}",
            "x".repeat(settings::MAX_PATCH_BYTES)
        );
        let (status, _) = post_settings(
            settings_app(store, loopback()),
            Some("application/json"),
            None,
            &huge,
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// A patch the validator refuses is answered `400` with the key named, and
    /// nothing is written or applied.
    #[tokio::test]
    async fn a_rejected_patch_is_answered_with_the_key_and_writes_nothing() {
        let cases = [
            (r#"{ "poll.poll_rate_hz": 0 }"#, "poll.poll_rate_hz"),
            (r#"{ "features.enable_pp": 5 }"#, "features.enable_pp"),
            (
                r#"{ "scoring.mod_multipliers": { "DTNC": 2.0 } }"#,
                "scoring.mod_multipliers",
            ),
            (r#"{ "server.overlay_dir": "x" }"#, "unknown key"),
            ("[]", "must be a JSON object"),
            ("not json at all", "not valid JSON"),
        ];

        for (body, expected) in cases {
            let store = settings_store("rejected");
            let (status, response_body) = post_settings(
                settings_app(store.clone(), loopback()),
                Some("application/json"),
                None,
                body,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let parsed: serde_json::Value =
                serde_json::from_str(&response_body).expect("the error is JSON, like tosu's");
            let message = parsed["error"].as_str().expect("an error message");
            assert!(
                message.contains(expected),
                "{body} must name {expected}, got: {message}"
            );
            assert!(!store.path().exists(), "{body} must not write the config");
            assert_eq!(store.config().poll.poll_rate_hz, 60);
        }
    }

    /// The landing page renders the settings form and the same overlay cards the
    /// `/overlays` dashboard does.
    #[tokio::test]
    async fn the_landing_page_shows_the_form_and_the_overlay_cards() {
        let store = settings_store("landing");
        let root = temp_overlay_root("landing");
        write_overlay_file(&root, "Team Bar/index.html", "<html>team bar</html>");

        let (_tx, rx) = watch::channel(PublishedPacket::default_packet());
        let app = create_router(
            AppState::with_overlays(rx, root.clone()).with_settings(store, "127.0.0.1", 24050),
            true,
            true,
            true,
        )
        .layer(MockConnectInfo(loopback()));

        let (status, body) = get_text(app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("data-key=\"poll.poll_rate_hz\""),
            "the form must render a control per setting"
        );
        assert!(body.contains("data-key=\"scoring.mod_multipliers\""));
        assert!(
            body.contains("/overlays/Team%20Bar/"),
            "the overlay card URL must be the one an OBS source uses"
        );
        assert!(body.contains("Team Bar"), "and the card names it");
        assert!(
            body.contains("/api/settings"),
            "the page's script posts to the API"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}

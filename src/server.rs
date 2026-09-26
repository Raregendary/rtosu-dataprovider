use crate::v2::TosuV2Packet;
use anyhow::{Context, Result};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
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
            .route("/health", get(handle_health));
    }

    if enable_ws {
        router = router
            .route("/websocket/v2", get(handle_ws_upgrade))
            .route("/websocket/v2/precise", get(handle_ws_upgrade));
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
    if published.packet.client == "none" {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "not_ready" })),
        )
            .into_response();
    }
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
/// Also configures graceful shutdown listening for termination signals.
pub async fn start_server(
    host: &str,
    port: u16,
    enable_http: bool,
    enable_ws: bool,
    cors_allow_all: bool,
    packet_rx: watch::Receiver<PublishedPacket>,
) -> Result<()> {
    if !enable_http && !enable_ws {
        tracing::info!(
            "Zero-port bypass active: both HTTP and WebSocket are disabled; TCP socket will not be bound."
        );
        return Ok(());
    }

    let listener = bind_listener(host, port).await?;
    serve_with_listener(listener, enable_http, enable_ws, cors_allow_all, packet_rx).await
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
    packet_rx: watch::Receiver<PublishedPacket>,
) -> Result<()> {
    let state = AppState { packet_rx };
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
        let state = AppState { packet_rx: rx };
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
        let state = AppState { packet_rx: rx };
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
        let state = AppState { packet_rx: rx };
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
        let app_cors_enabled = create_router(AppState { packet_rx: rx1 }, true, false, true);
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
        let app_cors_disabled = create_router(AppState { packet_rx: rx2 }, true, false, false);
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
        let res = start_server("127.0.0.1", 0, false, false, true, rx).await;
        assert!(res.is_ok());
    }
}

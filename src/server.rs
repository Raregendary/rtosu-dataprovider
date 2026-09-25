use crate::v2::TosuV2Packet;
use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use std::net::SocketAddr;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub packet_rx: watch::Receiver<TosuV2Packet>,
}

pub fn create_router(state: AppState, enable_http: bool, enable_ws: bool) -> Router {
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

    router
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn handle_json_v2(State(state): State<AppState>) -> impl IntoResponse {
    let packet = state.packet_rx.borrow().clone();
    if packet.client == "none" {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "not_ready" })),
        )
            .into_response();
    }
    Json(packet).into_response()
}

async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    let packet = state.packet_rx.borrow();
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

async fn handle_ws_stream(mut socket: WebSocket, mut packet_rx: watch::Receiver<TosuV2Packet>) {
    tracing::debug!("WebSocket client connected");

    // Send immediate initial state
    let initial_json = {
        let packet = packet_rx.borrow();
        serde_json::to_string(&*packet).unwrap_or_default()
    };

    if !initial_json.is_empty() {
        if socket.send(Message::Text(initial_json)).await.is_err() {
            tracing::debug!("WebSocket client disconnected during initial handshake");
            return;
        }
    }

    // Stream updates on each tick
    while packet_rx.changed().await.is_ok() {
        let json_str = {
            let packet = packet_rx.borrow();
            serde_json::to_string(&*packet).unwrap_or_default()
        };

        if socket.send(Message::Text(json_str)).await.is_err() {
            break;
        }
    }

    tracing::debug!("WebSocket client disconnected");
}

/// Start the tosu-compatible HTTP and WebSocket server.
/// If both `enable_http` and `enable_ws` are false, the function immediately
/// returns `Ok(())` without binding any TCP port (zero-port bypass).
pub async fn start_server(
    host: &str,
    port: u16,
    enable_http: bool,
    enable_ws: bool,
    packet_rx: watch::Receiver<TosuV2Packet>,
) -> Result<()> {
    if !enable_http && !enable_ws {
        tracing::info!(
            "Zero-port bypass active: both HTTP and WebSocket are disabled; TCP socket will not be bound."
        );
        return Ok(());
    }

    let state = AppState { packet_rx };
    let app = create_router(state, enable_http, enable_ws);

    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .with_context(|| format!("invalid host:port '{}:{}'", host, port))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding TCP listener to {}", addr))?;

    tracing::info!("Listening on TCP socket {}", addr);

    axum::serve(listener, app)
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

        let (tx, rx) = watch::channel(sample);
        let rx_holder = rx.clone();
        let state = AppState { packet_rx: rx };
        let app = create_router(state, true, true);

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
        tx.send(updated).unwrap();
        assert_eq!(rx_holder.borrow().client, "tournament");
    }

    #[tokio::test]
    async fn test_router_conditional_routes_http_only() {
        let mut sample = TosuV2Packet::default();
        sample.client = "stable".to_string();

        let (_tx, rx) = watch::channel(sample);
        let state = AppState { packet_rx: rx };
        let app = create_router(state, true, false);

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

        let (_tx, rx) = watch::channel(sample);
        let state = AppState { packet_rx: rx };
        let app = create_router(state, false, true);

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
    async fn test_start_server_zero_port_bypass() {
        let sample = TosuV2Packet::default();
        let (_tx, rx) = watch::channel(sample);

        // Even with an invalid or bound port, if both are false, it should succeed immediately
        let res = start_server("127.0.0.1", 0, false, false, rx).await;
        assert!(res.is_ok());
    }
}

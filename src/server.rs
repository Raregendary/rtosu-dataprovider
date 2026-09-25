use crate::v2::TosuV2Packet;
use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub packet_rx: watch::Receiver<TosuV2Packet>,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/json/v2", get(handle_json_v2))
        .route("/json", get(handle_json_v2))
        .route("/health", get(handle_health))
        .route("/websocket/v2", get(handle_ws_upgrade))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn handle_json_v2(State(state): State<AppState>) -> Json<TosuV2Packet> {
    let packet = state.packet_rx.borrow().clone();
    Json(packet)
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
    ws.on_upgrade(move |socket| handle_ws_stream(socket, state.packet_rx))
}

async fn handle_ws_stream(mut socket: WebSocket, mut packet_rx: watch::Receiver<TosuV2Packet>) {
    // Send immediate initial state
    let initial_json = {
        let packet = packet_rx.borrow();
        serde_json::to_string(&*packet).unwrap_or_default()
    };

    if !initial_json.is_empty() {
        if socket.send(Message::Text(initial_json)).await.is_err() {
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
}

/// Start the tosu-compatible HTTP and WebSocket server
pub async fn start_server(
    host: &str,
    port: u16,
    packet_rx: watch::Receiver<TosuV2Packet>,
) -> Result<()> {
    let state = AppState { packet_rx };
    let app = create_router(state);

    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .with_context(|| format!("invalid host:port '{}:{}'", host, port))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding TCP listener to {}", addr))?;

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
        let app = create_router(state);

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
}

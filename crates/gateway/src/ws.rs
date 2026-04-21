use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::state::AppState;
use crate::ws_registry::WsRegistry;

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct Challenge {
    #[serde(rename = "type")]
    msg_type: &'static str,
    nonce: String,
}

#[derive(Deserialize)]
struct AuthRequest {
    address: Address,
    signature: String,
    #[serde(default)]
    timestamp: u64,
}

#[derive(Serialize)]
struct AuthSuccess {
    #[serde(rename = "type")]
    msg_type: &'static str,
    address: Address,
}

#[derive(Serialize)]
struct AuthError {
    #[serde(rename = "type")]
    msg_type: &'static str,
    error: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    let (registry, domain_separator) = {
        let s = state.lock().await;
        (s.ws_registry.clone(), s.domain_separator)
    };

    ws.on_upgrade(move |socket| handle_socket(socket, registry, domain_separator))
}

async fn send_error(socket: &mut WebSocket, error: &str) {
    let err = AuthError {
        msg_type: "error",
        error: error.to_owned(),
    };
    let json = serde_json::to_string(&err).expect("AuthError serializes");
    let _ = socket.send(Message::Text(json.into())).await;
    let _ = socket.send(Message::Close(None)).await;
}

async fn handle_socket(
    mut socket: WebSocket,
    registry: Arc<RwLock<WsRegistry>>,
    domain_separator: B256,
) {
    let nonce = B256::from(rand::random::<[u8; 32]>());
    let challenge = Challenge {
        msg_type: "challenge",
        nonce: format!("0x{}", hex::encode(nonce)),
    };

    let challenge_json = serde_json::to_string(&challenge).expect("Challenge serializes");
    if socket
        .send(Message::Text(challenge_json.into()))
        .await
        .is_err()
    {
        return;
    }

    let auth_result = tokio::time::timeout(AUTH_TIMEOUT, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                Some(Ok(Message::Close(_))) | None => return None,
                _ => continue,
            }
        }
    })
    .await;

    let auth_text = match auth_result {
        Ok(Some(text)) => text,
        _ => {
            send_error(&mut socket, "auth timeout").await;
            return;
        }
    };

    let auth_req: AuthRequest = match serde_json::from_str(&auth_text) {
        Ok(req) => req,
        Err(e) => {
            send_error(&mut socket, &format!("invalid auth message: {e}")).await;
            return;
        }
    };

    let sig_bytes = match hex::decode(
        auth_req
            .signature
            .strip_prefix("0x")
            .unwrap_or(&auth_req.signature),
    ) {
        Ok(b) => b,
        Err(_) => {
            send_error(&mut socket, "invalid signature hex").await;
            return;
        }
    };

    let recovered =
        match crate::eip712::verify_auth(nonce, auth_req.timestamp, &sig_bytes, domain_separator) {
            Ok(addr) => addr,
            Err(e) => {
                warn!(error = %e, "ws auth verification failed");
                send_error(&mut socket, "signature verification failed").await;
                return;
            }
        };

    if recovered != auth_req.address {
        send_error(&mut socket, "address mismatch").await;
        return;
    }

    let success = AuthSuccess {
        msg_type: "authenticated",
        address: recovered,
    };
    if socket
        .send(Message::Text(
            serde_json::to_string(&success)
                .expect("AuthSuccess serializes")
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    info!(address = %recovered, "ws client authenticated");

    let (sender_id, mut rx) = {
        let mut reg = registry.write().await;
        reg.register(recovered)
    };

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(ws_msg) => {
                        let json = serde_json::to_string(&ws_msg).expect("WsMessage serializes");
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    let mut reg = registry.write().await;
    reg.remove(&recovered, sender_id);
}

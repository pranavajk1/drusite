// SPDX-License-Identifier: AGPL-3.0-or-later

//! The sync driver for native apps: one WebSocket to the server, running a [`Core`]'s sync
//! session until the connection ends. Reconnecting, and refreshing the access token first, is
//! the caller's loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use drusite_model::WorkspaceId;
use drusite_proto::{
    ClientMessage, Ping, SUBPROTOCOL, ServerMessage, client_message, server_message,
};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use url::Url;

use super::SyncError;
use crate::auth::{AuthError, DeviceKey, proof};
use crate::engine::Core;

/// How often an idle connection pings, so proxies keep it open.
const PING_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("can't connect to the server: {0}")]
    Connect(String),
    #[error("the server isn't a Drusite sync server it understands: {0}")]
    Protocol(String),
    #[error("the access token was refused; refresh it and connect again")]
    Unauthorized,
    #[error("signed out from the server")]
    SignedOut,
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("the core is unusable after a panic elsewhere")]
    Poisoned,
}

/// Where a server's sync endpoint is, as an `http(s)` URL (what a DPoP proof names) and as the
/// `ws(s)` URL to connect to.
pub fn endpoints(server: &Url) -> Result<(Url, Url), NetError> {
    let http = server
        .join("api/v1/sync")
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let mut ws = http.clone();
    let scheme = if http.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    ws.set_scheme(scheme)
        .map_err(|()| NetError::Connect("bad server URL".to_owned()))?;
    Ok((http, ws))
}

fn lock(core: &Mutex<Core>) -> Result<std::sync::MutexGuard<'_, Core>, NetError> {
    core.lock().map_err(|_| NetError::Poisoned)
}

/// Connects and syncs until the connection ends. `join` takes the account's workspace in place
/// of a blank device's own (see [`Core::sync_start_join`]). `now` is the clock, in seconds since
/// the Unix epoch, for the DPoP proof.
pub async fn run(
    core: Arc<Mutex<Core>>,
    server: &Url,
    access_token: &str,
    key: &impl DeviceKey,
    join: Option<WorkspaceId>,
    now: i64,
) -> Result<(), NetError> {
    let (http, ws_url) = endpoints(server)?;
    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let headers = request.headers_mut();
    let header =
        |v: &str| http::HeaderValue::from_str(v).map_err(|e| NetError::Connect(e.to_string()));
    headers.insert("authorization", header(&format!("DPoP {access_token}"))?);
    headers.insert(
        "dpop",
        header(&proof(key, "GET", &http, Some(access_token), now)?)?,
    );
    headers.insert("sec-websocket-protocol", header(SUBPROTOCOL)?);

    let (mut socket, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(response))
            if response.status() == http::StatusCode::UNAUTHORIZED =>
        {
            return Err(NetError::Unauthorized);
        }
        Err(e) => return Err(NetError::Connect(e.to_string())),
    };

    // The server speaks first.
    match socket.next().await {
        Some(Ok(Message::Binary(frame))) => {
            let welcome: ServerMessage =
                drusite_proto::decode(&frame).map_err(|e| NetError::Protocol(e.to_string()))?;
            if !matches!(welcome.body, Some(server_message::Body::Welcome(_))) {
                return Err(NetError::Protocol("no welcome".to_owned()));
            }
        }
        other => return Err(NetError::Protocol(format!("no welcome: {other:?}"))),
    }

    let changed = Arc::new(Notify::new());
    let first = {
        let mut core = lock(&core)?;
        let notify = Arc::clone(&changed);
        core.set_change_listener(Some(Box::new(move || notify.notify_one())));
        match join {
            Some(ws) => core.sync_start_join(ws),
            None => core.sync_start(),
        }
        .map_err(SyncError::from)?
    };

    let result = async {
        send_all(&mut socket, first).await?;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut nonce = 0;
        loop {
            tokio::select! {
                incoming = socket.next() => {
                    let frame = match incoming {
                        Some(Ok(Message::Binary(frame))) => frame,
                        Some(Ok(Message::Close(frame))) => {
                            return match frame {
                                Some(f) if u16::from(f.code) == 4001 => Err(NetError::SignedOut),
                                _ => Ok(()),
                            };
                        }
                        None => return Ok(()),
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => return Err(NetError::Connect(e.to_string())),
                    };
                    let message: ServerMessage = drusite_proto::decode(&frame)
                        .map_err(|e| NetError::Protocol(e.to_string()))?;
                    let replies = lock(&core)?.sync_receive(message)?;
                    send_all(&mut socket, replies).await?;
                }
                () = changed.notified() => {
                    let out = lock(&core)?.sync_outgoing().map_err(SyncError::from)?;
                    send_all(&mut socket, out).await?;
                }
                _ = ping.tick() => {
                    nonce += 1;
                    send_all(&mut socket, vec![ClientMessage::new(client_message::Body::Ping(Ping { nonce }))]).await?;
                }
            }
        }
    }
    .await;

    if let Ok(mut core) = core.lock() {
        core.sync_stop();
        core.set_change_listener(None);
    }
    let _ = socket.close(None).await;
    result
}

async fn send_all<S>(socket: &mut S, messages: Vec<ClientMessage>) -> Result<(), NetError>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    for message in messages {
        socket
            .send(Message::Binary(drusite_proto::encode(&message).into()))
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
    }
    Ok(())
}

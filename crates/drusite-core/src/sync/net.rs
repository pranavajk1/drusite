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
/// A connection that has heard nothing from the server for this long is dead (a half-open
/// TCP connection after the network changed, say), even though nothing has failed: drop it, so
/// the caller reconnects.
const SILENT_FOR: Duration = Duration::from_secs(75);
/// How long one send may take. A write to a dead connection can block for as long as the OS
/// keeps retrying (many minutes), and nothing else in the loop runs meanwhile.
const SEND_WITHIN: Duration = Duration::from_secs(20);

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

    // Reading never waits on writing: outgoing messages go through a queue to a writer task.
    // Otherwise both ends can block writing to each other (a device sending many subscriptions
    // while the server sends catch-ups), and neither ever reads again.
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut queue) = tokio::sync::mpsc::unbounded_channel::<ClientMessage>();
    let mut writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            send_one(&mut sink, &message).await?;
        }
        let _ = sink.close().await;
        Ok::<(), NetError>(())
    });
    let queue_all = |messages: Vec<ClientMessage>| {
        for m in messages {
            // The writer only stops after an error, which the loop below picks up.
            let _ = outgoing.send(m);
        }
    };

    let result = async {
        queue_all(first);
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut nonce = 0;
        let mut heard = tokio::time::Instant::now();
        loop {
            tokio::select! {
                incoming = stream.next() => {
                    heard = tokio::time::Instant::now();
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
                    queue_all(lock(&core)?.sync_receive(message)?);
                }
                () = changed.notified() => {
                    queue_all(lock(&core)?.sync_outgoing().map_err(SyncError::from)?);
                }
                _ = ping.tick() => {
                    if heard.elapsed() > SILENT_FOR {
                        return Err(NetError::Connect("the server stopped answering".to_owned()));
                    }
                    nonce += 1;
                    queue_all(vec![ClientMessage::new(client_message::Body::Ping(Ping { nonce }))]);
                    queue_all(lock(&core)?.sync_retry().map_err(SyncError::from)?);
                }
                ended = &mut writer => {
                    return match ended {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(NetError::Connect(e.to_string())),
                    };
                }
            }
        }
    }
    .await;
    writer.abort();

    if let Ok(mut core) = core.lock() {
        core.sync_stop();
        core.set_change_listener(None);
    }
    result
}

async fn send_one<S>(sink: &mut S, message: &ClientMessage) -> Result<(), NetError>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    tokio::time::timeout(
        SEND_WITHIN,
        sink.send(Message::Binary(drusite_proto::encode(message).into())),
    )
    .await
    .map_err(|_| NetError::Connect("the connection stopped taking data".to_owned()))?
    .map_err(|e| NetError::Connect(e.to_string()))
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ask your notes, through the server: every app asks the same way. The server finds the
//! blocks, writes the answer with its chat model and keeps the conversations, so a phone, a
//! desktop and the browser all see the same ones.

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use drusite_model::{BlockId, PageId};
use serde::Deserialize;
use url::Url;

use super::{AccountError, HttpError, Supervisor, agent, auth, blocking, now, read};
use crate::api::{
    AskMessage, AskRole, AskSource, AskStatus, AskUpdate, Conversation, ConversationSummary,
};

/// The longest an answer may stream (the server's own limit is five minutes by default).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Answers being followed, so the UI can stop following one.
fn following() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static FOLLOWING: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    FOLLOWING.get_or_init(Mutex::default)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatus {
    available: bool,
    reason: Option<String>,
    message: Option<String>,
    model: Option<String>,
    semantic: bool,
    /// Older servers leave it out.
    #[serde(default)]
    embedding_model: Option<String>,
    indexing: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSummary {
    id: String,
    title: String,
    updated_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSource {
    n: u32,
    page: PageId,
    block: Option<BlockId>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    missing: bool,
}

impl From<ServerSource> for AskSource {
    fn from(s: ServerSource) -> Self {
        Self {
            n: s.n,
            page: s.page,
            block: s.block,
            title: s.title,
            missing: s.missing,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerMessage {
    id: String,
    role: String,
    text: String,
    sources: Vec<ServerSource>,
    model: Option<String>,
    error: Option<String>,
    at: i64,
}

#[derive(Deserialize)]
struct ServerConversation {
    id: String,
    title: String,
    messages: Vec<ServerMessage>,
}

fn not_signed_in() -> AskStatus {
    AskStatus {
        signed_in: false,
        available: false,
        message: Some("Sign in to a Drusite server to ask your notes.".to_owned()),
        reason: Some("not_signed_in".to_owned()),
        model: None,
        semantic: false,
        embedding_model: None,
        indexing: 0,
    }
}

/// A failed call: the server's error code when it gave one, and words for the person.
pub(super) struct Failure {
    pub(super) code: Option<String>,
    pub(super) message: String,
}

impl From<HttpError> for Failure {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Refused { code, description } => Self {
                message: if description.is_empty() {
                    code.clone()
                } else {
                    description
                },
                code: Some(code),
            },
            other => Self {
                code: None,
                message: AccountError::from(other).to_string(),
            },
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}

impl Supervisor {
    async fn send_json<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, String> {
        self.call(method, path, body).await.map_err(|f| f.message)
    }

    pub(super) async fn call<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, Failure> {
        let (server, token) = self.current()?;
        let url = server.join(path).map_err(|e| e.to_string())?;
        let mut htu = url.clone();
        htu.set_query(None);
        let proof = auth::proof(&*self.inner.key, method, &htu, Some(&token), now())
            .map_err(|e| e.to_string())?;
        let authorization = format!("DPoP {token}");
        let agent = agent();
        blocking(move || {
            let builder = http::Request::builder()
                .method(method)
                .uri(url.as_str())
                .header("Authorization", &authorization)
                .header("DPoP", &proof);
            let request = match body {
                Some(b) => builder
                    .header("Content-Type", "application/json")
                    .body(b.to_string()),
                None => builder.body(String::new()),
            }
            .map_err(|e| HttpError::Network(e.to_string()))?;
            read(agent.run(request))
        })
        .await
        .map_err(Failure::from)
    }

    pub(super) fn ws_path(&self, rest: &str) -> Result<String, String> {
        Ok(format!("api/v1/workspaces/{}/{rest}", self.workspace()?))
    }

    /// Whether Ask works with this device's server.
    pub async fn ask_status(&self) -> AskStatus {
        if self.current().is_err() {
            let mut status = not_signed_in();
            if self.status().server.is_some() {
                status.signed_in = true;
                status.reason = Some("unreachable".to_owned());
                status.message = Some("Not connected to the server right now.".to_owned());
            }
            return status;
        }
        let path = match self.workspace() {
            Ok(ws) => format!("api/v1/ask?workspace={ws}"),
            Err(e) => {
                let mut s = not_signed_in();
                s.message = Some(e);
                return s;
            }
        };
        match self.call::<ServerStatus>("GET", &path, None).await {
            Ok(s) => AskStatus {
                signed_in: true,
                available: s.available,
                message: s.message,
                reason: s.reason,
                model: s.model,
                semantic: s.semantic,
                embedding_model: s.embedding_model,
                indexing: u32::try_from(s.indexing.max(0)).unwrap_or(u32::MAX),
            },
            // A server from before Ask answers 404.
            Err(f) if matches!(f.code.as_deref(), Some("404" | "not_found")) => AskStatus {
                signed_in: true,
                available: false,
                message: Some(
                    "This server is older than Ask your notes; it needs updating.".to_owned(),
                ),
                reason: Some("old_server".to_owned()),
                model: None,
                semantic: false,
                embedding_model: None,
                indexing: 0,
            },
            // The workspace hasn't reached the server yet (a moment after signing in).
            Err(f) if f.code.as_deref() == Some("forbidden") => AskStatus {
                signed_in: true,
                available: false,
                message: Some("Your notes are still syncing with the server.".to_owned()),
                reason: Some("syncing".to_owned()),
                model: None,
                semantic: false,
                embedding_model: None,
                indexing: 0,
            },
            Err(f) => AskStatus {
                signed_in: true,
                available: false,
                message: Some(f.message),
                reason: Some("unreachable".to_owned()),
                model: None,
                semantic: false,
                embedding_model: None,
                indexing: 0,
            },
        }
    }

    /// The saved conversations, most recent first.
    pub async fn conversations(&self) -> Result<Vec<ConversationSummary>, String> {
        let list: Vec<ServerSummary> = self
            .send_json("GET", &self.ws_path("conversations")?, None)
            .await?;
        Ok(list
            .into_iter()
            .map(|c| ConversationSummary {
                id: c.id,
                title: c.title,
                updated_at: c.updated_at,
            })
            .collect())
    }

    pub async fn conversation(&self, id: &str) -> Result<Conversation, String> {
        let c: ServerConversation = self
            .send_json("GET", &self.ws_path(&format!("conversations/{id}"))?, None)
            .await?;
        Ok(Conversation {
            id: c.id,
            title: c.title,
            messages: c
                .messages
                .into_iter()
                .map(|m| AskMessage {
                    id: m.id,
                    role: if m.role == "user" {
                        AskRole::User
                    } else {
                        AskRole::Assistant
                    },
                    text: m.text,
                    sources: m.sources.into_iter().map(Into::into).collect(),
                    model: m.model,
                    error: m.error,
                    at: m.at,
                })
                .collect(),
        })
    }

    pub async fn rename_conversation(&self, id: &str, title: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .send_json(
                "PATCH",
                &self.ws_path(&format!("conversations/{id}"))?,
                Some(serde_json::json!({ "title": title })),
            )
            .await?;
        Ok(())
    }

    pub async fn delete_conversation(&self, id: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .send_json(
                "DELETE",
                &self.ws_path(&format!("conversations/{id}"))?,
                None,
            )
            .await?;
        Ok(())
    }

    /// Stops following an answer.
    pub fn cancel_ask(request: &str) {
        if let Ok(f) = following().lock()
            && let Some(flag) = f.get(request)
        {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Asks a question. The answer is passed to `on_update` as it streams in, from a thread of
    /// its own; this returns once the request is on its way (or refused).
    pub async fn ask(
        &self,
        request: String,
        conversation: Option<String>,
        question: String,
        today: Option<String>,
        on_update: impl Fn(&str, AskUpdate) + Send + 'static,
    ) -> Result<(), String> {
        let (server, token) = self.current()?;
        let url = server
            .join(&self.ws_path("ask")?)
            .map_err(|e| e.to_string())?;
        let proof = auth::proof(&*self.inner.key, "POST", &url, Some(&token), now())
            .map_err(|e| e.to_string())?;
        let body = serde_json::json!({
            "conversation": conversation,
            "question": question,
            "today": today,
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Ok(mut f) = following().lock() {
            f.insert(request.clone(), Arc::clone(&cancelled));
        }
        let authorization = format!("DPoP {token}");
        tokio::task::spawn_blocking(move || {
            let result = stream(&url, &authorization, &proof, &body, &cancelled, &|u| {
                on_update(&request, u);
            });
            if let Err(message) = result {
                on_update(&request, AskUpdate::Failed { message });
            }
            if let Ok(mut f) = following().lock() {
                f.remove(&request);
            }
        });
        Ok(())
    }
}

/// Reads the server-sent events of one answer, until it ends, fails or is cancelled.
fn stream(
    url: &Url,
    authorization: &str,
    proof: &str,
    body: &serde_json::Value,
    cancelled: &AtomicBool,
    on_update: &dyn Fn(AskUpdate),
) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ANSWER_TIMEOUT))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url.as_str())
        .header("Authorization", authorization)
        .header("DPoP", proof)
        .header("Accept", "text/event-stream")
        .send_json(body)
        .map_err(|e| format!("can't reach the server: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        #[derive(Deserialize)]
        struct Refusal {
            message: Option<String>,
        }
        let refusal: Option<Refusal> = response.body_mut().read_json().ok();
        return Err(refusal
            .and_then(|r| r.message)
            .unwrap_or_else(|| format!("the server answered {status}")));
    }
    let reader = std::io::BufReader::new(response.body_mut().as_reader());
    let mut name = String::new();
    let mut data = String::new();
    let mut ended = false;
    for line in reader.lines() {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(());
        }
        let line = line.map_err(|e| format!("the answer stopped: {e}"))?;
        if line.is_empty() {
            if !name.is_empty() {
                ended |= dispatch(&name, &data, on_update);
            }
            name.clear();
            data.clear();
            if ended {
                return Ok(());
            }
        } else if let Some(n) = line.strip_prefix("event:") {
            n.trim().clone_into(&mut name);
        } else if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    if ended {
        Ok(())
    } else {
        Err("the answer stopped partway; it may still be saved in the conversation".to_owned())
    }
}

/// Turns one event into an update; true for the last one.
fn dispatch(name: &str, data: &str, on_update: &dyn Fn(AskUpdate)) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    match name {
        "started" => on_update(AskUpdate::Started {
            conversation: text("conversation"),
            title: text("title"),
            message: text("message"),
        }),
        "sources" => {
            let sources: Vec<ServerSource> = serde_json::from_value(v).unwrap_or_default();
            on_update(AskUpdate::Sources {
                sources: sources.into_iter().map(Into::into).collect(),
            });
        }
        "text" => on_update(AskUpdate::Text { text: text("text") }),
        "done" => {
            on_update(AskUpdate::Done {
                message: v["message"].as_str().map(str::to_owned),
                cited: v["cited"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_u64().and_then(|n| u32::try_from(n).ok()))
                            .collect()
                    })
                    .unwrap_or_default(),
                model: v["model"].as_str().map(str::to_owned),
            });
            return true;
        }
        "error" => {
            on_update(AskUpdate::Failed {
                message: text("message"),
            });
            return true;
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_updates() {
        let seen = Mutex::new(Vec::new());
        let push = |u: AskUpdate| seen.lock().unwrap_or_else(|e| e.into_inner()).push(u);
        assert!(!dispatch("text", r#"{"text":"Hi [1]"}"#, &push));
        assert!(!dispatch("ping", "{}", &push));
        assert!(dispatch(
            "done",
            r#"{"message":"m","cited":[1,2],"model":"q"}"#,
            &push
        ));
        let seen = seen.into_inner().unwrap_or_default();
        assert_eq!(
            seen,
            [
                AskUpdate::Text {
                    text: "Hi [1]".into()
                },
                AskUpdate::Done {
                    message: Some("m".into()),
                    cited: vec![1, 2],
                    model: Some("q".into())
                }
            ]
        );
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! `POST /api/v1/workspaces/{ws}/ask`: one question, answered as a stream of server-sent
//! events.
//!
//! Events, each with JSON data:
//! - `started` `{conversation, title, message}`: the question is saved (`message` is its ID).
//! - `sources` `[{n, page, block, title}]`: the blocks the answer may cite, by number.
//! - `text` `{text}`: more of the answer.
//! - `done` `{message, cited, model, stop}`: the answer is saved; `cited` are the numbers it
//!   cited.
//! - `error` `{error, message}`: it stopped; whatever came before is saved.
//!
//! The answer is written by a task of its own, so a connection that drops (a phone going to
//! sleep) doesn't lose it: it's saved, and the conversation shows it when opened again.

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use drusite_ai::{ChatEvent, ChatRequest, Message, Role};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::audit::{self, Call};
use super::conversations::{self, NewMessage, SourceRef};
use super::prompt::{self, Source, Thoughts};
use super::retrieve::{self, Found, Scope};
use super::{AskError, Chat, readable_pages, require_reader};
use crate::app::AppState;
use crate::auth::Authenticated;

/// The longest question taken, in characters.
const MAX_QUESTION: usize = 4_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AskBody {
    /// Continues this conversation; a new one if none.
    conversation: Option<Uuid>,
    question: String,
    /// The asker's local date (`YYYY-MM-DD`), so "yesterday" means theirs.
    today: Option<String>,
    /// Keeps the answer to this page and the pages inside it: asked from that page.
    #[serde(default)]
    page: Option<Uuid>,
}

fn event(name: &str, data: &serde_json::Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

fn valid_day(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

pub(super) async fn ask(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(ws): Path<Uuid>,
    Json(body): Json<AskBody>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AskError> {
    let config = state.config();
    if !config.ask.enabled {
        return Err(AskError::Unavailable(
            "disabled",
            "Ask is turned off on this server.".to_owned(),
        ));
    }
    if !config.search.enabled {
        return Err(AskError::Unavailable(
            "search_off",
            "Ask needs server search, which is turned off on this server.".to_owned(),
        ));
    }
    if state.chat().is_none() {
        return Err(AskError::Unavailable(
            "no_chat_model",
            "This server has no chat model yet; see deploy/README.md.".to_owned(),
        ));
    }
    let question = body.question.trim().to_owned();
    if question.is_empty() || question.chars().count() > MAX_QUESTION {
        return Err(AskError::Invalid(format!(
            "a question has 1 to {MAX_QUESTION} characters"
        )));
    }
    let today = body
        .today
        .filter(|d| valid_day(d))
        .unwrap_or_else(|| time::OffsetDateTime::now_utc().date().to_string());
    let user = *auth.user.as_uuid();

    // The limits count every node's questions, so they're read as the system.
    {
        let mut tx = state.db().as_system().await?;
        audit::check_limits(&state, &mut tx, user).await?;
        tx.commit().await?;
    }

    let mut tx = state.db().as_user(auth.user).await?;
    require_reader(&state, &mut tx, auth.user, ws).await?;
    let mut readable = readable_pages(&state, &mut tx, ws)
        .await?
        .ok_or(AskError::NotFound)?;
    if let Some(page) = body.page {
        readable.within(page);
    }
    let (conversation, title, history) = match body.conversation {
        Some(id) => {
            let title = conversations::find(&state, &mut tx, ws, id)
                .await?
                .ok_or(AskError::NotFound)?;
            let history = conversations::messages(&state, &mut tx, ws, id).await?;
            (id, title, history)
        }
        None => {
            let title = conversations::title_of(&question);
            let id = conversations::create(&state, &mut tx, ws, user, &title).await?;
            (id, title, Vec::new())
        }
    };
    let asked = conversations::add_message(
        &state,
        &mut tx,
        &NewMessage {
            conversation,
            ws,
            user,
            role: "user",
            text: &question,
            sources: &[],
            model: None,
            error: None,
        },
    )
    .await?;
    tx.commit().await?;

    let (send, receive) = mpsc::channel::<Event>(64);
    let _ = send
        .send(event(
            "started",
            &json!({ "conversation": conversation, "title": title, "message": asked }),
        ))
        .await;
    let job = Job {
        state: state.clone(),
        auth_user: auth.user,
        ws,
        user,
        conversation,
        question,
        today,
        history,
        readable,
    };
    tokio::spawn(async move {
        job.run(send).await;
    });
    let stream = futures_util::stream::unfold(receive, |mut r| async move {
        r.recv().await.map(|e| (Ok(e), r))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

struct Job {
    state: AppState,
    auth_user: drusite_model::UserId,
    ws: Uuid,
    user: Uuid,
    conversation: Uuid,
    question: String,
    today: String,
    history: Vec<conversations::Stored>,
    readable: super::Readable,
}

/// How an answer ended.
struct Ending {
    text: String,
    sources: Vec<SourceRef>,
    error: Option<(&'static str, String)>,
}

impl Job {
    async fn run(self, send: mpsc::Sender<Event>) {
        let ending = match self.answer(&send).await {
            Ok(e) => e,
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), "answering a question");
                Ending {
                    text: String::new(),
                    sources: Vec::new(),
                    error: Some((
                        "server_error",
                        "Something went wrong on the server.".to_owned(),
                    )),
                }
            }
        };
        let chat = self.state.chat();
        let saved = async {
            let mut tx = self.state.db().as_user(self.auth_user).await?;
            let id = conversations::add_message(
                &self.state,
                &mut tx,
                &NewMessage {
                    conversation: self.conversation,
                    ws: self.ws,
                    user: self.user,
                    role: "assistant",
                    text: &ending.text,
                    sources: &ending.sources,
                    model: chat.map(|c| c.name.as_str()),
                    error: ending.error.as_ref().map(|(_, m)| m.as_str()),
                },
            )
            .await?;
            tx.commit().await?;
            anyhow::Ok(id)
        }
        .await;
        let message = match saved {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), "saving an answer");
                None
            }
        };
        let last = match &ending.error {
            Some((code, m)) => event(
                "error",
                &json!({ "error": code, "message": m, "message_id": message }),
            ),
            None => event(
                "done",
                &json!({
                    "message": message,
                    "cited": ending.sources.iter().map(|s| s.n).collect::<Vec<_>>(),
                    "model": chat.map(|c| c.name.as_str()),
                }),
            ),
        };
        let _ = send.send(last).await;
    }

    /// The vector of what's being asked, recorded as sent.
    async fn embed_question(&self, text: &str) -> Option<Vec<f32>> {
        let embedder = self.state.embedder()?;
        let call = Call {
            workspace: Some(self.ws),
            user: Some(self.user),
            feature: "ask-embed",
            provider: embedder.provider(),
            model: embedder.model_id(),
            blocks: json!([]),
            items: 1,
            chars: text.chars().count(),
        };
        let id = {
            let mut tx = self.state.db().as_user(self.auth_user).await.ok()?;
            let id = audit::begin(&mut tx, &call).await.ok()?;
            tx.commit().await.ok()?;
            id
        };
        match embedder.embed(&[text.to_owned()]).await {
            Ok(mut v) => {
                audit::finish(&self.state, id, "ok", None, None).await;
                v.pop()
            }
            Err(e) => {
                tracing::warn!(
                    error = audit::outcome_of(&e),
                    "embedding a question; searching words only"
                );
                audit::finish(&self.state, id, audit::outcome_of(&e), None, None).await;
                None
            }
        }
    }

    async fn answer(&self, send: &mpsc::Sender<Event>) -> anyhow::Result<Ending> {
        let state = &self.state;
        let config = &state.config().ask;
        let Some(chat) = state.chat() else {
            anyhow::bail!("no chat model");
        };

        // A follow-up ("and the year before?") is searched with the question before it.
        let previous = self
            .history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text.clone());
        let search_text = match &previous {
            Some(p) => format!("{p}\n{}", self.question),
            None => self.question.clone(),
        };
        let vector = self.embed_question(&search_text).await;
        let found: Vec<Found> = {
            let mut tx = state.db().as_user(self.auth_user).await?;
            let scope = Scope {
                workspace: self.ws,
                pages: &self.readable.pages,
                excluded: &self.readable.excluded,
                values: &self.readable.values,
            };
            let found =
                retrieve::find(state, &mut tx, &scope, &search_text, vector.as_deref()).await?;
            tx.commit().await?;
            found
        };
        let sources: Vec<Source> = found
            .iter()
            .enumerate()
            .map(|(i, f)| Source {
                n: i + 1,
                page_title: f.title.clone(),
                text: f.text.clone(),
            })
            .collect();
        let refs: Vec<SourceRef> = found
            .iter()
            .enumerate()
            .map(|(i, f)| SourceRef {
                n: i + 1,
                page: f.page,
                block: f.block,
            })
            .collect();
        let _ = send
            .send(event(
                "sources",
                &json!(
                    found
                        .iter()
                        .enumerate()
                        .map(|(i, f)| json!({
                            "n": i + 1, "page": f.page, "block": f.block, "title": f.title,
                        }))
                        .collect::<Vec<_>>()
                ),
            ))
            .await;

        // The conversation so far, then this question with its sources.
        let tag = prompt::random_tag();
        let mut messages = Vec::new();
        let turns = config.history_turns * 2;
        let start = self.history.len().saturating_sub(turns);
        for m in &self.history[start..] {
            match m.role.as_str() {
                "user" => messages.push(Message::user(m.text.clone())),
                _ if !m.text.trim().is_empty() => {
                    messages.push(Message::assistant(prompt::without_citations(&m.text)));
                }
                _ => {}
            }
        }
        // Providers want turns to alternate, starting with the user's.
        let mut alternating: Vec<Message> = Vec::new();
        for m in messages {
            match alternating.last_mut() {
                Some(last) if last.role == m.role => {
                    last.content.push_str("\n\n");
                    last.content.push_str(&m.content);
                }
                None if m.role == Role::Assistant => {}
                _ => alternating.push(m),
            }
        }
        if alternating.last().is_some_and(|m| m.role == Role::User) {
            alternating.push(Message::assistant("(no answer)"));
        }
        let user_turn = prompt::question(&tag, &sources, &self.question);
        let chars = user_turn.chars().count();
        alternating.push(Message::user(user_turn));
        let request = ChatRequest {
            model: chat.model.clone(),
            system: Some(prompt::system(&tag, &self.today)),
            messages: alternating,
            max_tokens: chat.max_tokens,
            temperature: chat.temperature,
        };

        let audit_id = {
            let mut tx = state.db().as_user(self.auth_user).await?;
            let id = audit::begin(
                &mut tx,
                &Call {
                    workspace: Some(self.ws),
                    user: Some(self.user),
                    feature: "ask",
                    provider: &chat.provider,
                    model: &chat.model,
                    blocks: json!(
                        found
                            .iter()
                            .map(|f| json!([f.page, f.block]))
                            .collect::<Vec<_>>()
                    ),
                    items: found.len(),
                    chars,
                },
            )
            .await?;
            tx.commit().await?;
            id
        };

        let (text, outcome, usage, error) = self.stream(chat, &request, send).await;
        audit::finish(state, audit_id, outcome, usage.input, usage.output).await;
        let cited = prompt::cited(&text, refs.len());
        let sources = refs.into_iter().filter(|r| cited.contains(&r.n)).collect();
        Ok(Ending {
            text,
            sources,
            error,
        })
    }

    /// Streams the answer to the client; returns its text, how it went, and the tokens used.
    async fn stream(
        &self,
        chat: &Chat,
        request: &ChatRequest,
        send: &mpsc::Sender<Event>,
    ) -> (
        String,
        &'static str,
        drusite_ai::Usage,
        Option<(&'static str, String)>,
    ) {
        let config = &self.state.config().ask;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(config.timeout_secs);
        let idle = Duration::from_secs(config.idle_secs);
        let mut text = String::new();
        let mut thoughts = Thoughts::default();
        let mut usage = drusite_ai::Usage::default();
        let failed = |e: &drusite_ai::AiError| {
            tracing::warn!(error = audit::outcome_of(e), "the chat model failed");
            (
                audit::outcome_of(e),
                Some(("provider", format!("The chat model failed: {e}"))),
            )
        };
        let mut stream = match tokio::time::timeout_at(deadline, chat.provider.chat(request)).await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                let (o, err) = failed(&e);
                return (text, o, usage, err);
            }
            Err(_) => return (text, "timeout", usage, Some(timeout())),
        };
        loop {
            // Until the first words, a reasoning model may think quietly for a while; after
            // that, a long silence means it's stuck.
            let wait = if text.is_empty() {
                deadline
            } else {
                deadline.min(tokio::time::Instant::now() + idle)
            };
            match tokio::time::timeout_at(wait, stream.next()).await {
                Err(_) => {
                    text.push_str(&thoughts.finish());
                    return (text, "timeout", usage, Some(timeout()));
                }
                Ok(None) => {
                    text.push_str(&thoughts.finish());
                    let e = drusite_ai::AiError::Malformed("the answer stopped partway".into());
                    let (o, err) = failed(&e);
                    return (text, o, usage, err);
                }
                Ok(Some(Err(e))) => {
                    text.push_str(&thoughts.finish());
                    let (o, err) = failed(&e);
                    return (text, o, usage, err);
                }
                Ok(Some(Ok(ChatEvent::Text(t)))) => {
                    let shown = thoughts.push(&t);
                    if !shown.is_empty() {
                        text.push_str(&shown);
                        let _ = send.send(event("text", &json!({ "text": shown }))).await;
                    }
                }
                Ok(Some(Ok(ChatEvent::Done { usage: u, stop }))) => {
                    usage = u;
                    let rest = thoughts.finish();
                    if !rest.is_empty() {
                        text.push_str(&rest);
                        let _ = send.send(event("text", &json!({ "text": rest }))).await;
                    }
                    if text.trim().is_empty() {
                        let why = if stop.as_deref() == Some("length")
                            || stop.as_deref() == Some("max_tokens")
                        {
                            "The model used all its tokens before answering (its thinking counts). Raise [ask.chat] max_tokens."
                        } else {
                            "The model gave an empty answer."
                        };
                        return (text, "empty", usage, Some(("empty", why.to_owned())));
                    }
                    return (text, "ok", usage, None);
                }
            }
        }
    }
}

fn timeout() -> (&'static str, String) {
    (
        "timeout",
        "The chat model took too long to answer.".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days() {
        assert!(valid_day("2026-11-27"));
        assert!(!valid_day("2026-1-27"));
        assert!(!valid_day("27/11/2026"));
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Chats, streamed: each vendor's request body and stream format, behind one interface.

use std::collections::VecDeque;
use std::pin::Pin;

use futures_util::{Stream, StreamExt};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::AiError;
use crate::provider::{Dialect, Kind, Provider};
use crate::sse::{Decoder, SseEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    /// Instructions for the whole conversation.
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// The longest answer, in tokens. Anthropic requires one; the others take it as a cap.
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

/// Tokens used, as far as the provider says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: Option<u32>,
    pub output: Option<u32>,
}

/// A piece of a streamed answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// More of the answer's text.
    Text(String),
    /// The answer is complete. Always the last event.
    Done {
        /// Why it stopped, in the provider's words (`stop`, `end_turn`, `length`, …).
        stop: Option<String>,
        usage: Usage,
    },
}

/// An answer, whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub stop: Option<String>,
    pub usage: Usage,
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, AiError>> + Send>>;

impl Provider {
    /// Starts a chat; the answer streams in as [`ChatEvent`]s, ending with [`ChatEvent::Done`]
    /// (or an error).
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatStream, AiError> {
        self.chat_with(request, None).await
    }

    pub(crate) async fn chat_with(
        &self,
        request: &ChatRequest,
        timeout: Option<std::time::Duration>,
    ) -> Result<ChatStream, AiError> {
        if request.model.trim().is_empty() {
            return Err(AiError::Invalid("pick a chat model".to_owned()));
        }
        let (url, body) = match self.kind().dialect() {
            Dialect::OpenAi => (
                self.url("chat/completions")?,
                openai_body(self.kind(), request),
            ),
            Dialect::Anthropic => (self.url("messages")?, anthropic_body(request)),
            Dialect::Google => {
                let model = request
                    .model
                    .strip_prefix("models/")
                    .unwrap_or(&request.model);
                let mut url = self.url(&format!("models/{model}:streamGenerateContent"))?;
                url.query_pairs_mut().append_pair("alt", "sse");
                (url, google_body(request))
            }
        };
        let mut builder = self
            .request(Method::POST, url)
            .header("accept", "text/event-stream")
            .json(&body);
        if let Some(t) = timeout {
            builder = builder.timeout(t);
        }
        let response = self.send(builder).await?;
        let bytes = response.bytes_stream().boxed();
        let state = State {
            bytes: Some(bytes),
            decoder: Decoder::default(),
            parser: Parser::new(self.kind().dialect()),
            queue: VecDeque::new(),
            done: false,
        };
        Ok(futures_util::stream::unfold(state, next).boxed())
    }

    /// A chat, collected into one answer.
    pub async fn complete(&self, request: &ChatRequest) -> Result<Reply, AiError> {
        collect(self.chat(request).await?).await
    }
}

pub(crate) async fn collect(mut stream: ChatStream) -> Result<Reply, AiError> {
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            ChatEvent::Text(t) => text.push_str(&t),
            ChatEvent::Done { stop, usage } => return Ok(Reply { text, stop, usage }),
        }
    }
    Err(AiError::Malformed("the answer stopped partway".to_owned()))
}

fn openai_body(kind: Kind, r: &ChatRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(s) = &r.system {
        // OpenAI's newer models call it "developer" but still take "system"; so does
        // every compatible server.
        messages.push(json!({ "role": "system", "content": s }));
    }
    for m in &r.messages {
        messages.push(json!({ "role": m.role, "content": m.content }));
    }
    let mut body = json!({ "model": r.model, "messages": messages, "stream": true });
    match kind {
        // OpenAI's reasoning models refuse max_tokens; everything else still knows only it.
        Kind::OpenAi => {
            body["max_completion_tokens"] = json!(r.max_tokens);
            body["stream_options"] = json!({ "include_usage": true });
        }
        Kind::OpenRouter => {
            body["max_tokens"] = json!(r.max_tokens);
            body["usage"] = json!({ "include": true });
        }
        _ => body["max_tokens"] = json!(r.max_tokens),
    }
    if let Some(t) = r.temperature {
        body["temperature"] = json!(t);
    }
    body
}

fn anthropic_body(r: &ChatRequest) -> Value {
    let messages: Vec<_> = r
        .messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    let mut body = json!({
        "model": r.model,
        "max_tokens": r.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if let Some(s) = &r.system {
        body["system"] = json!(s);
    }
    if let Some(t) = r.temperature {
        body["temperature"] = json!(t);
    }
    body
}

fn google_body(r: &ChatRequest) -> Value {
    let contents: Vec<_> = r
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            json!({ "role": role, "parts": [{ "text": m.content }] })
        })
        .collect();
    let mut config = json!({ "maxOutputTokens": r.max_tokens });
    if let Some(t) = r.temperature {
        config["temperature"] = json!(t);
    }
    let mut body = json!({ "contents": contents, "generationConfig": config });
    if let Some(s) = &r.system {
        body["systemInstruction"] = json!({ "parts": [{ "text": s }] });
    }
    body
}

type Bytes = Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

struct State {
    bytes: Option<Bytes>,
    decoder: Decoder,
    parser: Parser,
    queue: VecDeque<Result<ChatEvent, AiError>>,
    done: bool,
}

async fn next(mut s: State) -> Option<(Result<ChatEvent, AiError>, State)> {
    loop {
        if let Some(item) = s.queue.pop_front() {
            if item.is_err() || matches!(item, Ok(ChatEvent::Done { .. })) {
                s.done = true;
                s.queue.clear();
                s.bytes = None;
            }
            return Some((item, s));
        }
        if s.done {
            return None;
        }
        let bytes = s.bytes.as_mut()?;
        match bytes.next().await {
            Some(Ok(chunk)) => {
                for event in s.decoder.push(&chunk) {
                    s.parser.event(&event, &mut s.queue);
                }
            }
            Some(Err(e)) => s.queue.push_back(Err(AiError::network(&e))),
            None => {
                if let Some(event) = s.decoder.finish() {
                    s.parser.event(&event, &mut s.queue);
                }
                s.bytes = None;
                if !s
                    .queue
                    .iter()
                    .any(|e| matches!(e, Ok(ChatEvent::Done { .. }) | Err(_)))
                {
                    s.queue.push_back(s.parser.end());
                }
            }
        }
    }
}

/// Turns one vendor's events into [`ChatEvent`]s.
struct Parser {
    dialect: Dialect,
    stop: Option<String>,
    usage: Usage,
    /// Some text arrived, or the provider said it was finished.
    finished: bool,
    any: bool,
}

impl Parser {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            stop: None,
            usage: Usage::default(),
            finished: false,
            any: false,
        }
    }

    fn done(&mut self) -> Result<ChatEvent, AiError> {
        self.finished = true;
        Ok(ChatEvent::Done {
            stop: self.stop.take(),
            usage: self.usage,
        })
    }

    /// The stream ended without the provider's end marker.
    fn end(&mut self) -> Result<ChatEvent, AiError> {
        match self.dialect {
            // Google has no end marker: the last chunk carries the finish reason.
            Dialect::Google if self.any => self.done(),
            // Some compatible servers close without "[DONE]" once they've said why they
            // stopped.
            Dialect::OpenAi if self.stop.is_some() => self.done(),
            _ => Err(AiError::Malformed("the answer stopped partway".to_owned())),
        }
    }

    fn event(&mut self, event: &SseEvent, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
        if self.finished {
            return;
        }
        let data = event.data.trim();
        if data.is_empty() {
            return;
        }
        if self.dialect == Dialect::OpenAi && data == "[DONE]" {
            out.push_back(self.done());
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            out.push_back(Err(AiError::Malformed(format!(
                "a piece of the answer isn't JSON: “{}”",
                data.chars().take(80).collect::<String>()
            ))));
            return;
        };
        // An error partway through: OpenAI-style servers, OpenRouter, Anthropic and Google all
        // put it under "error".
        if !v["error"].is_null() || event.event.as_deref() == Some("error") {
            let message = crate::error::message_of(data);
            let status = v["error"]["code"]
                .as_u64()
                .and_then(|c| u16::try_from(c).ok())
                .unwrap_or(500);
            out.push_back(Err(AiError::from_status(
                status,
                &json!({"message": message}).to_string(),
            )));
            return;
        }
        match self.dialect {
            Dialect::OpenAi => self.openai(&v, out),
            Dialect::Anthropic => self.anthropic(&v, out),
            Dialect::Google => self.google(&v, out),
        }
    }

    fn openai(&mut self, v: &Value, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
        if let Some(choice) = v["choices"].get(0) {
            // Reasoning (`reasoning_content`, `reasoning`) isn't part of the answer.
            if let Some(t) = choice["delta"]["content"].as_str()
                && !t.is_empty()
            {
                self.any = true;
                out.push_back(Ok(ChatEvent::Text(t.to_owned())));
            }
            if let Some(r) = choice["finish_reason"].as_str() {
                self.stop = Some(r.to_owned());
            }
        }
        if v["usage"].is_object() {
            self.usage.input = u32_of(&v["usage"]["prompt_tokens"]).or(self.usage.input);
            self.usage.output = u32_of(&v["usage"]["completion_tokens"]).or(self.usage.output);
        }
    }

    fn anthropic(&mut self, v: &Value, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
        match v["type"].as_str() {
            Some("message_start") => {
                self.usage.input = u32_of(&v["message"]["usage"]["input_tokens"]);
            }
            Some("content_block_delta") => {
                // Thinking and tool-input deltas aren't part of the answer.
                if v["delta"]["type"] == "text_delta"
                    && let Some(t) = v["delta"]["text"].as_str()
                {
                    self.any = true;
                    out.push_back(Ok(ChatEvent::Text(t.to_owned())));
                }
            }
            Some("message_delta") => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(r.to_owned());
                }
                if let Some(n) = u32_of(&v["usage"]["output_tokens"]) {
                    self.usage.output = Some(n);
                }
            }
            Some("message_stop") => out.push_back(self.done()),
            _ => {}
        }
    }

    fn google(&mut self, v: &Value, out: &mut VecDeque<Result<ChatEvent, AiError>>) {
        self.any = true;
        if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
            self.stop = Some(format!("blocked: {reason}"));
        }
        if let Some(candidate) = v["candidates"].get(0) {
            for part in candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if part["thought"].as_bool() == Some(true) {
                    continue;
                }
                if let Some(t) = part["text"].as_str()
                    && !t.is_empty()
                {
                    out.push_back(Ok(ChatEvent::Text(t.to_owned())));
                }
            }
            if let Some(r) = candidate["finishReason"].as_str() {
                self.stop = Some(r.to_owned());
            }
        }
        if v["usageMetadata"].is_object() {
            self.usage.input = u32_of(&v["usageMetadata"]["promptTokenCount"]);
            self.usage.output = u32_of(&v["usageMetadata"]["candidatesTokenCount"]);
        }
    }
}

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

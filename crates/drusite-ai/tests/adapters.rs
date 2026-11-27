// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every adapter against a fake of its vendor's API: where the key goes, what the request looks
//! like, how the stream comes back (split anywhere), and what an error looks like. The custom
//! endpoint is faked as each self-hosted server answers, quirks included.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use drusite_ai::{
    AiError, ChatEvent, ChatRequest, EmbeddingModel, Embeddings, Kind, Message, Provider, check,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use url::Url;
use zeroize::Zeroizing;

/// What the fake answers with, and what it saw.
#[derive(Default)]
struct Fake {
    /// (status, content type, body, chunk size for streaming).
    answer: Mutex<Option<(u16, &'static str, String, usize)>>,
    seen: Mutex<Vec<(String, HeaderMap, Value)>>,
}

async fn handler(State(fake): State<Arc<Fake>>, request: Request) -> Response {
    let path = request.uri().to_string();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    fake.seen.lock().unwrap().push((path, headers, body));
    let (status, kind, text, chunk) = fake.answer.lock().unwrap().clone().unwrap();
    // Sent in small pieces, to split events (and UTF-8 characters) across chunks.
    let pieces: Vec<Result<Vec<u8>, std::io::Error>> = text
        .into_bytes()
        .chunks(chunk.max(1))
        .map(|c| Ok(c.to_vec()))
        .collect();
    let mut response =
        Response::new(Body::from_stream(futures_util::stream::iter(pieces))).into_response();
    *response.status_mut() = StatusCode::from_u16(status).unwrap();
    response
        .headers_mut()
        .insert("content-type", kind.parse().unwrap());
    response
}

struct Server {
    fake: Arc<Fake>,
    base: Url,
}

impl Server {
    async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let fake = Arc::new(Fake::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
        let app = Router::new()
            .fallback(handler)
            .with_state(Arc::clone(&fake));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { fake, base }
    }

    fn answer(&self, status: u16, kind: &'static str, body: impl Into<String>, chunk: usize) {
        *self.fake.answer.lock().unwrap() = Some((status, kind, body.into(), chunk));
    }

    fn json(&self, body: Value) {
        self.answer(200, "application/json", body.to_string(), 1 << 20);
    }

    fn sse(&self, events: &[&str], chunk: usize) {
        let text: String = events.iter().map(|e| format!("{e}\n\n")).collect();
        self.answer(200, "text/event-stream", text, chunk);
    }

    fn last(&self) -> (String, HeaderMap, Value) {
        self.fake.seen.lock().unwrap().last().cloned().unwrap()
    }

    fn provider(&self, kind: Kind, key: Option<&str>) -> Provider {
        Provider::new(
            reqwest::Client::new(),
            kind,
            Some(&self.base),
            key.map(|k| Zeroizing::new(k.to_owned())),
        )
        .unwrap()
    }
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        system: Some("Be brief.".to_owned()),
        messages: vec![
            Message::user("Hi"),
            Message::assistant("Hello"),
            Message::user("Say ok"),
        ],
        max_tokens: 100,
        temperature: Some(0.2),
    }
}

/// Every event of a chat.
async fn events(p: &Provider, model: &str) -> Vec<Result<ChatEvent, AiError>> {
    match p.chat(&request(model)).await {
        Ok(stream) => stream.collect().await,
        Err(e) => vec![Err(e)],
    }
}

fn text(events: &[Result<ChatEvent, AiError>]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Ok(ChatEvent::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn done(events: &[Result<ChatEvent, AiError>]) -> (Option<String>, Option<u32>, Option<u32>) {
    match events.last() {
        Some(Ok(ChatEvent::Done { stop, usage })) => (stop.clone(), usage.input, usage.output),
        other => panic!("ended with {other:?}"),
    }
}

const OPENAI_STREAM: &[&str] = &[
    r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
    r#"data: {"choices":[{"index":0,"delta":{"reasoning_content":"hmm"}}]}"#,
    r#"data: {"choices":[{"index":0,"delta":{"content":"O"}}]}"#,
    r#"data: {"choices":[{"index":0,"delta":{"content":"K é"},"finish_reason":"stop"}]}"#,
    r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#,
    "data: [DONE]",
];

#[tokio::test]
async fn openai_streams_with_its_own_token_limit_and_usage() {
    let s = Server::start().await;
    let p = s.provider(Kind::OpenAi, Some("sk-1"));
    for chunk in [1, 7, 1 << 20] {
        s.sse(OPENAI_STREAM, chunk);
        let ev = events(&p, "gpt-x").await;
        assert_eq!(text(&ev), "OK é", "chunk {chunk}");
        assert_eq!(done(&ev), (Some("stop".into()), Some(12), Some(3)));
    }
    let (path, headers, body) = s.last();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(headers["authorization"], "Bearer sk-1");
    assert_eq!(body["max_completion_tokens"], 100);
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(
        body["messages"][0],
        json!({"role":"system","content":"Be brief."})
    );
    assert_eq!(
        body["messages"][2],
        json!({"role":"assistant","content":"Hello"})
    );
    let t = body["temperature"].as_f64().unwrap();
    assert!((t - 0.2).abs() < 1e-6);
}

#[tokio::test]
async fn compatible_servers_get_max_tokens_and_may_skip_the_end_marker() {
    let s = Server::start().await;
    // No key: a local Ollama or llama.cpp.
    let p = s.provider(Kind::OpenAiCompatible, None);
    // Ends without [DONE] after a finish reason, as some servers do.
    s.sse(&OPENAI_STREAM[..4], 5);
    let ev = events(&p, "qwen").await;
    assert_eq!(text(&ev), "OK é");
    assert_eq!(done(&ev).0.as_deref(), Some("stop"));
    let (_, headers, body) = s.last();
    assert!(headers.get("authorization").is_none());
    assert_eq!(body["max_tokens"], 100);
    assert!(body.get("stream_options").is_none());

    // Cut off before any finish reason: an error, not a silently short answer.
    s.sse(&OPENAI_STREAM[..3], 5);
    let ev = events(&p, "qwen").await;
    assert!(
        matches!(ev.last(), Some(Err(AiError::Malformed(_)))),
        "{ev:?}"
    );

    // An error partway through the stream (vLLM, LiteLLM).
    s.sse(
        &[
            OPENAI_STREAM[2],
            r#"data: {"error":{"message":"context too long","code":400}}"#,
        ],
        3,
    );
    let ev = events(&p, "qwen").await;
    assert_eq!(text(&ev), "O");
    assert_eq!(
        ev.last(),
        Some(&Err(AiError::Refused {
            status: 400,
            message: "context too long".into()
        }))
    );

    // Ollama's error shape, before the stream starts.
    s.answer(
        404,
        "application/json",
        r#"{"error":"model \"x\" not found"}"#,
        1 << 20,
    );
    let ev = events(&p, "x").await;
    assert_eq!(
        ev,
        vec![Err(AiError::NotFound("model \"x\" not found".into()))]
    );
}

#[tokio::test]
async fn openrouter_sends_its_title_and_ignores_keepalive_comments() {
    let s = Server::start().await;
    let p = s.provider(Kind::OpenRouter, Some("or-1"));
    let mut stream = vec![": OPENROUTER PROCESSING"];
    stream.extend_from_slice(OPENAI_STREAM);
    s.sse(&stream, 4);
    let ev = events(&p, "anthropic/claude").await;
    assert_eq!(text(&ev), "OK é");
    let (_, headers, body) = s.last();
    assert_eq!(headers["x-title"], "Drusite");
    assert_eq!(headers["authorization"], "Bearer or-1");
    assert_eq!(body["usage"]["include"], true);
}

#[tokio::test]
async fn mistral_uses_max_tokens_and_its_own_dimension_field() {
    let s = Server::start().await;
    let p = s.provider(Kind::Mistral, Some("m-1"));
    s.sse(OPENAI_STREAM, 9);
    assert_eq!(text(&events(&p, "mistral-small").await), "OK é");
    assert_eq!(s.last().2["max_tokens"], 100);

    s.json(json!({"data":[{"index":0,"embedding":[0.5,0.25]}]}));
    let v = p
        .embed("codestral-embed", &["a".into()], Some(2))
        .await
        .unwrap();
    assert_eq!(v, vec![vec![0.5, 0.25]]);
    let (path, _, body) = s.last();
    assert_eq!(path, "/v1/embeddings");
    assert_eq!(body["output_dimension"], 2);
    assert!(body.get("dimensions").is_none());
}

#[tokio::test]
async fn anthropic_streams_its_events_and_skips_thinking() {
    let s = Server::start().await;
    let p = s.provider(Kind::Anthropic, Some("ak-1"));
    s.sse(
        &[
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}",
            "event: ping\ndata: {\"type\":\"ping\"}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"O\"}}",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"K é\"}}",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}",
        ],
        6,
    );
    let ev = events(&p, "claude-x").await;
    assert_eq!(text(&ev), "OK é");
    assert_eq!(done(&ev), (Some("end_turn".into()), Some(20), Some(4)));
    let (path, headers, body) = s.last();
    assert_eq!(path, "/v1/messages");
    assert_eq!(headers["x-api-key"], "ak-1");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(headers.get("authorization").is_none());
    assert_eq!(body["system"], "Be brief.");
    assert_eq!(body["max_tokens"], 100);
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);

    // An overloaded error partway through.
    s.sse(
        &["event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}"],
        1 << 20,
    );
    let ev = events(&p, "claude-x").await;
    assert!(
        matches!(ev.last(), Some(Err(AiError::Refused { message, .. })) if message == "Overloaded"),
        "{ev:?}"
    );

    // No embeddings, said plainly.
    assert!(matches!(
        p.embed("x", &["a".into()], None).await,
        Err(AiError::Unsupported(_))
    ));
    assert!(EmbeddingModel::new(p.clone(), "x", None, None).is_err());

    s.json(json!({"data":[{"id":"claude-b","display_name":"Claude B"},{"id":"claude-a"}],"has_more":false}));
    let models = p.models().await.unwrap();
    assert_eq!(models[0].id, "claude-a");
    assert_eq!(models[1].name.as_deref(), Some("Claude B"));
    assert_eq!(s.last().0, "/v1/models?limit=1000");
}

#[tokio::test]
async fn google_streams_generate_content_and_batches_embeddings() {
    let s = Server::start().await;
    let p = s.provider(Kind::Google, Some("g-1"));
    s.sse(
        &[
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"thinking","thought":true},{"text":"O"}]}}]}"#,
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"K é"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2}}"#,
        ],
        5,
    );
    let ev = events(&p, "models/gemini-x").await;
    assert_eq!(text(&ev), "OK é");
    assert_eq!(done(&ev), (Some("STOP".into()), Some(9), Some(2)));
    let (path, headers, body) = s.last();
    assert_eq!(path, "/v1/models/gemini-x:streamGenerateContent?alt=sse");
    assert_eq!(headers["x-goog-api-key"], "g-1");
    assert!(!path.contains("g-1"), "the key never goes in the URL");
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be brief.");
    assert_eq!(body["contents"][1]["role"], "model");
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 100);

    s.json(json!({"embeddings":[{"values":[1.0,2.0]},{"values":[3.0,4.0]}]}));
    let m = EmbeddingModel::new(p.clone(), "gemini-embedding", None, Some(2)).unwrap();
    assert_eq!(m.model_id(), "gemini-embedding");
    let v = m.embed(&["a".into(), "b".into()]).await.unwrap();
    assert_eq!(v, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    let (path, _, body) = s.last();
    assert_eq!(path, "/v1/models/gemini-embedding:batchEmbedContents");
    assert_eq!(body["requests"][1]["model"], "models/gemini-embedding");
    assert_eq!(body["requests"][1]["outputDimensionality"], 2);

    s.json(json!({"models":[
        {"name":"models/gemini-x","displayName":"Gemini X","supportedGenerationMethods":["generateContent","countTokens"]},
        {"name":"models/gemini-embedding","supportedGenerationMethods":["embedContent"]},
        {"name":"models/imagen","supportedGenerationMethods":["predict"]}
    ]}));
    let models = p.models().await.unwrap();
    let ids: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.embeddings))
        .collect();
    assert_eq!(ids, [("gemini-embedding", true), ("gemini-x", false)]);

    s.answer(
        400,
        "application/json",
        r#"{"error":{"code":400,"message":"API key not valid.","status":"INVALID_ARGUMENT"}}"#,
        1 << 20,
    );
    assert!(matches!(
        p.models().await,
        Err(AiError::Refused { status: 400, message }) if message == "API key not valid."
    ));
}

#[tokio::test]
async fn embeddings_come_back_in_order_and_are_checked() {
    let s = Server::start().await;
    let p = s.provider(Kind::OpenAi, Some("sk"));
    let m = EmbeddingModel::new(p.clone(), "tiny", Some("tiny-v1".into()), None).unwrap();
    assert_eq!(m.model_id(), "tiny-v1");
    s.json(json!({"data":[{"index":1,"embedding":[3.0,1.0]},{"index":0,"embedding":[1.0,1.0]}]}));
    assert_eq!(
        m.embed(&["a".into(), "abc".into()]).await.unwrap(),
        vec![vec![1.0, 1.0], vec![3.0, 1.0]]
    );
    let (_, _, body) = s.last();
    assert!(body.get("dimensions").is_none());
    assert!(m.embed(&[]).await.unwrap().is_empty());

    // A server that leaves the index out (some llama.cpp builds).
    s.json(json!({"data":[{"embedding":[1.0]},{"embedding":[2.0]}]}));
    let p = s.provider(Kind::OpenAiCompatible, None);
    assert_eq!(
        p.embed("e", &["a".into(), "b".into()], None).await.unwrap(),
        vec![vec![1.0], vec![2.0]]
    );
    s.json(json!({"data":[{"index":0,"embedding":[1.0]}]}));
    assert!(matches!(
        p.embed("e", &["a".into(), "b".into()], None).await,
        Err(AiError::Malformed(_))
    ));
    s.json(json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[1.0,2.0]}]}));
    assert!(matches!(
        p.embed("e", &["a".into(), "b".into()], None).await,
        Err(AiError::Malformed(_))
    ));
    s.answer(502, "text/html", "<html>Bad Gateway</html>", 1 << 20);
    assert!(matches!(
        p.embed("e", &["a".into()], None).await,
        Err(AiError::Refused { status: 502, .. })
    ));
}

#[tokio::test]
async fn the_connection_check_names_what_failed() {
    let s = Server::start().await;
    let p = s.provider(Kind::OpenAiCompatible, None);

    // A refused key stops at the first step.
    s.answer(
        401,
        "application/json",
        r#"{"error":{"message":"bad key"}}"#,
        1 << 20,
    );
    let steps = check(&p, Some("chat"), Some("embed")).await;
    assert_eq!(steps.len(), 1);
    assert!(!steps[0].ok);
    assert!(steps[0].detail.contains("bad key"), "{}", steps[0].detail);

    // Models listed; the chat answers.
    s.json(json!({"data":[{"id":"chat"},{"id":"nomic-embed-text"}]}));
    let models = p.models().await.unwrap();
    assert!(models[1].embeddings && !models[0].embeddings);
    let steps = check(&p, None, None).await;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].detail, "It offers 2 models");

    s.sse(OPENAI_STREAM, 1 << 20);
    let steps = check(&p, Some("chat"), None).await;
    // The fake answers the models call with the stream too, so that step fails; the chat
    // is still tried.
    assert_eq!(steps.len(), 2);
    assert!(steps[1].ok, "{steps:?}");
    assert_eq!(steps[1].what, "Chat with chat");
    assert_eq!(steps[1].detail, "It answered “OK é”");
}

#[tokio::test]
async fn an_unreachable_endpoint_says_so_without_leaking_the_url() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = Url::parse(&format!(
        "http://{}/v1?key=secret",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    drop(listener);
    let p = Provider::new(
        reqwest::Client::new(),
        Kind::OpenAiCompatible,
        Some(&base),
        None,
    )
    .unwrap();
    let err = p.models().await.unwrap_err();
    assert!(matches!(err, AiError::Network(_)), "{err:?}");
    assert!(!err.to_string().contains("secret"), "{err}");
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! AI providers on a device: saved with their keys (never shown again), picked per feature, and
//! reached for models and *Test connection* against a fake self-hosted endpoint.

#![cfg(feature = "ai")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, post};
use drusite_core::api::{
    AiChoice, AiFeature, AiKind, ApiError, Command, ErrorCode, Event, Response,
};
use drusite_core::{Core, StoreKey};
use serde_json::{Value, json};

fn open(dir: &std::path::Path, key: &StoreKey, events: Arc<Mutex<Vec<Event>>>) -> Core {
    Core::open(dir, key, Box::new(move |e| events.lock().unwrap().push(e))).unwrap()
}

fn ok(core: &mut Core, command: Command) -> Response {
    core.handle(command).unwrap()
}

fn err(core: &mut Core, command: Command) -> ApiError {
    core.handle(command).unwrap_err()
}

/// A fake Ollama: models, a streamed chat and embeddings, all needing the key `local-key`.
async fn fake() -> String {
    async fn models(headers: axum::http::HeaderMap) -> (axum::http::StatusCode, axum::Json<Value>) {
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer local-key") {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "no key"})),
            );
        }
        (
            axum::http::StatusCode::OK,
            axum::Json(json!({"object":"list","data":[{"id":"qwen3"},{"id":"nomic-embed-text"}]})),
        )
    }
    async fn chat() -> ([(&'static str, &'static str); 1], &'static str) {
        (
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )
    }
    async fn embed(axum::Json(body): axum::Json<Value>) -> axum::Json<Value> {
        let n = body["input"].as_array().unwrap().len();
        let data: Vec<_> = (0..n)
            .map(|i| json!({"index": i, "embedding": [0.1, 0.2, 0.3]}))
            .collect();
        axum::Json(json!({ "data": data }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embed));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_are_saved_picked_and_tested() {
    let dir = tempfile::tempdir().unwrap();
    let key = StoreKey::generate().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut core = open(dir.path(), &key, Arc::clone(&events));
    let base = fake().await;

    let Response::Ai {
        available,
        kinds,
        providers,
        features,
    } = ok(&mut core, Command::Ai)
    else {
        panic!()
    };
    assert!(available);
    assert_eq!(kinds.len(), 6);
    assert!(
        !kinds
            .iter()
            .find(|k| k.kind == AiKind::Anthropic)
            .unwrap()
            .embeds
    );
    assert!(providers.is_empty());
    assert!(features.iter().all(|f| f.choice.is_none()));

    // A vendor needs a key; a custom endpoint needs an address that is one.
    let e = err(
        &mut core,
        Command::SaveAiProvider {
            id: None,
            kind: AiKind::OpenAi,
            name: String::new(),
            base_url: None,
            api_key: None,
        },
    );
    assert_eq!(e.code, ErrorCode::Invalid);
    assert!(e.message.contains("API key"), "{}", e.message);
    for bad in [None, Some("ftp://x"), Some("not a url")] {
        let e = err(
            &mut core,
            Command::SaveAiProvider {
                id: None,
                kind: AiKind::OpenAiCompatible,
                name: "Box".into(),
                base_url: bad.map(str::to_owned),
                api_key: None,
            },
        );
        assert_eq!(e.code, ErrorCode::Invalid, "{bad:?}");
    }

    let Response::AiProviderSaved { provider: local } = ok(
        &mut core,
        Command::SaveAiProvider {
            id: None,
            kind: AiKind::OpenAiCompatible,
            name: " Ollama ".into(),
            base_url: Some(base.clone()),
            api_key: Some("local-key".into()),
        },
    ) else {
        panic!()
    };
    assert_eq!(local.name, "Ollama");
    assert_eq!(local.key_hint.as_deref(), Some("…-key"));
    let Response::AiProviderSaved { provider: claude } = ok(
        &mut core,
        Command::SaveAiProvider {
            id: None,
            kind: AiKind::Anthropic,
            name: String::new(),
            base_url: None,
            api_key: Some("sk-ant-1234".into()),
        },
    ) else {
        panic!()
    };
    assert_eq!(claude.name, "Anthropic");
    assert!(events.lock().unwrap().contains(&Event::AiChanged));

    // Per feature. Anthropic can't embed.
    let e = err(
        &mut core,
        Command::SetAiFeature {
            feature: AiFeature::Embeddings,
            choice: Some(AiChoice {
                provider: claude.id.clone(),
                model: "x".into(),
            }),
        },
    );
    assert!(e.message.contains("embeddings"), "{}", e.message);
    ok(
        &mut core,
        Command::SetAiFeature {
            feature: AiFeature::Chat,
            choice: Some(AiChoice {
                provider: claude.id.clone(),
                model: "claude-x".into(),
            }),
        },
    );
    ok(
        &mut core,
        Command::SetAiFeature {
            feature: AiFeature::Embeddings,
            choice: Some(AiChoice {
                provider: local.id.clone(),
                model: "nomic-embed-text".into(),
            }),
        },
    );
    let (_, model) = core.ai_for(AiFeature::Embeddings).unwrap().unwrap();
    assert_eq!(model, "nomic-embed-text");

    // Renaming keeps the key when none is given.
    ok(
        &mut core,
        Command::SaveAiProvider {
            id: Some(local.id.clone()),
            kind: AiKind::OpenAiCompatible,
            name: "Ollama on the box".into(),
            base_url: Some(base.clone()),
            api_key: None,
        },
    );

    // Reaching the provider: models, then the check, through `run`.
    assert_eq!(
        err(
            &mut core,
            Command::AiModels {
                provider: local.id.clone()
            }
        )
        .code,
        ErrorCode::Unsupported
    );
    drop(core);
    let core = Arc::new(Mutex::new(open(dir.path(), &key, Arc::clone(&events))));
    let Response::AiModels { models } = drusite_core::ai::run(
        &core,
        Command::AiModels {
            provider: local.id.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    let ids: Vec<_> = models
        .iter()
        .map(|m| (m.id.as_str(), m.embeddings))
        .collect();
    assert_eq!(ids, [("nomic-embed-text", true), ("qwen3", false)]);

    let Response::AiCheck { steps } = drusite_core::ai::run(
        &core,
        Command::TestAiProvider {
            provider: local.id.clone(),
            chat_model: Some("qwen3".into()),
            embedding_model: Some("nomic-embed-text".into()),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert!(steps.iter().all(|s| s.ok), "{steps:?}");
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[2].detail, "It made a vector of 3 numbers");

    // A wrong key is named as the problem.
    {
        let mut c = core.lock().unwrap();
        ok(
            &mut c,
            Command::SaveAiProvider {
                id: Some(local.id.clone()),
                kind: AiKind::OpenAiCompatible,
                name: "Ollama on the box".into(),
                base_url: Some(base.clone()),
                api_key: Some(String::new()),
            },
        );
    }
    let Response::AiCheck { steps } = drusite_core::ai::run(
        &core,
        Command::TestAiProvider {
            provider: local.id.clone(),
            chat_model: Some("qwen3".into()),
            embedding_model: None,
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(steps.len(), 1);
    assert!(
        !steps[0].ok && steps[0].detail.contains("refused the key"),
        "{steps:?}"
    );

    // Removing a provider leaves its features without one; the rest stay.
    let mut c = core.lock().unwrap();
    ok(
        &mut c,
        Command::DeleteAiProvider {
            id: local.id.clone(),
        },
    );
    let Response::Ai {
        providers,
        features,
        ..
    } = ok(&mut c, Command::Ai)
    else {
        panic!()
    };
    assert_eq!(providers.len(), 1);
    let choice = |f: AiFeature| {
        features
            .iter()
            .find(|x| x.feature == f)
            .unwrap()
            .choice
            .clone()
    };
    assert!(choice(AiFeature::Embeddings).is_none());
    assert_eq!(choice(AiFeature::Chat).unwrap().model, "claude-x");
}

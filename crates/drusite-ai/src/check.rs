// SPDX-License-Identifier: AGPL-3.0-or-later

//! *Test connection*: does this provider work, with this key and these models?
//!
//! Each step says what it tried and what came back, so a failure points at the fix: the address,
//! the key, or the model's name.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::chat::{ChatRequest, Message, collect};
use crate::error::AiError;
use crate::provider::Provider;

/// One step of the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStep {
    /// What was tried: "Models", "Chat with …", "Embeddings with …".
    pub what: String,
    pub ok: bool,
    /// What came back, in a sentence.
    pub detail: String,
    pub millis: u64,
}

const CHAT_TIMEOUT: Duration = Duration::from_secs(60);

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Lists the models, then tries `chat_model` and `embedding_model` with a tiny request each.
/// A refused key stops it after the first step.
pub async fn check(
    provider: &Provider,
    chat_model: Option<&str>,
    embedding_model: Option<&str>,
) -> Vec<CheckStep> {
    let mut steps = Vec::new();
    let started = Instant::now();
    let models = provider.models().await;
    let known: Option<Vec<String>> = models
        .as_ref()
        .ok()
        .map(|m| m.iter().map(|m| m.id.clone()).collect());
    steps.push(CheckStep {
        what: "Models".to_owned(),
        ok: models.is_ok(),
        detail: match &models {
            Ok(m) if m.is_empty() => "It answered, with no models listed".to_owned(),
            Ok(m) if m.len() == 1 => "It offers 1 model".to_owned(),
            Ok(m) => format!("It offers {} models", m.len()),
            Err(e) => e.to_string(),
        },
        millis: millis(started),
    });
    if matches!(models, Err(AiError::Unauthorized(_) | AiError::Invalid(_))) {
        return steps;
    }
    let unlisted = |model: &str| {
        known
            .as_ref()
            .is_some_and(|k| !k.is_empty() && !k.iter().any(|id| id == model))
    };

    if let Some(model) = chat_model.map(str::trim).filter(|m| !m.is_empty()) {
        let started = Instant::now();
        let request = ChatRequest {
            model: model.to_owned(),
            system: None,
            messages: vec![Message::user("Reply with the single word: OK")],
            // Room for models that think before they answer.
            max_tokens: 256,
            temperature: None,
        };
        let result = match provider.chat_with(&request, Some(CHAT_TIMEOUT)).await {
            Ok(stream) => collect(stream).await,
            Err(e) => Err(e),
        };
        let mut detail = match &result {
            Ok(reply) => {
                let text: String = reply.text.trim().chars().take(60).collect();
                if text.is_empty() {
                    "It answered, with no text (the model may have spent its tokens thinking)"
                        .to_owned()
                } else {
                    format!("It answered “{text}”")
                }
            }
            Err(e) => e.to_string(),
        };
        if result.is_err() && unlisted(model) {
            detail.push_str(". The provider doesn't list this model: check its name");
        }
        steps.push(CheckStep {
            what: format!("Chat with {model}"),
            ok: result.is_ok(),
            detail,
            millis: millis(started),
        });
    }

    if let Some(model) = embedding_model.map(str::trim).filter(|m| !m.is_empty()) {
        let started = Instant::now();
        let result = provider
            .embed(model, &["Drusite connection test".to_owned()], None)
            .await;
        let mut detail = match &result {
            Ok(v) => format!("It made a vector of {} numbers", v[0].len()),
            Err(e) => e.to_string(),
        };
        if result.is_err() && unlisted(model) {
            detail.push_str(". The provider doesn't list this model: check its name");
        }
        steps.push(CheckStep {
            what: format!("Embeddings with {model}"),
            ok: result.is_ok(),
            detail,
            millis: millis(started),
        });
    }
    steps
}

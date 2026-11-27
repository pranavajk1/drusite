// SPDX-License-Identifier: AGPL-3.0-or-later

//! Text embeddings: the vectors semantic search compares.
//!
//! Every vector is stored with the ID of the model that made it ([`Embeddings::model_id`]):
//! vectors from different models live in different spaces and can't be compared, so changing
//! the model means embedding everything again.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("can't reach the embedding endpoint: {0}")]
    Network(String),
    #[error("the embedding endpoint refused the request ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("the embedding endpoint answered oddly: {0}")]
    Malformed(String),
}

/// Something that turns texts into vectors.
pub trait Embeddings: Send + Sync {
    /// Names the vector space: the same ID always means comparable vectors.
    fn model_id(&self) -> &str;

    /// One vector per text, in order.
    fn embed(
        &self,
        texts: &[String],
    ) -> impl std::future::Future<Output = Result<Vec<Vec<f32>>, EmbeddingError>> + Send;
}

/// An OpenAI-compatible `/embeddings` endpoint.
#[derive(Clone)]
pub struct OpenAiCompatible {
    http: reqwest::Client,
    url: Url,
    key: Option<Zeroizing<String>>,
    model: String,
    id: String,
    dimensions: Option<u32>,
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("url", &self.url.as_str())
            .field("model", &self.model)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
    encoding_format: &'static str,
}

#[derive(Deserialize)]
struct Answer {
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    index: usize,
    embedding: Vec<f32>,
}

impl OpenAiCompatible {
    /// `base` is the API's base URL (`https://api.openai.com/v1`, `http://litellm:4000/v1`, …).
    /// `id` names the vector space; by default it's the model name.
    pub fn new(
        base: &Url,
        model: impl Into<String>,
        key: Option<Zeroizing<String>>,
        id: Option<String>,
        dimensions: Option<u32>,
    ) -> Result<Self, EmbeddingError> {
        let mut url = base.clone();
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        let url = url
            .join("embeddings")
            .map_err(|e| EmbeddingError::Malformed(e.to_string()))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| EmbeddingError::Network(e.to_string()))?;
        let model = model.into();
        Ok(Self {
            http,
            url,
            key,
            id: id.unwrap_or_else(|| model.clone()),
            model,
            dimensions,
        })
    }
}

impl Embeddings for OpenAiCompatible {
    fn model_id(&self) -> &str {
        &self.id
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut request = self.http.post(self.url.clone()).json(&Request {
            model: &self.model,
            input: texts,
            dimensions: self.dimensions,
            encoding_format: "float",
        });
        if let Some(key) = &self.key {
            request = request.bearer_auth(key.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|e| EmbeddingError::Network(e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let mut message = response.text().await.unwrap_or_default();
            message.truncate(500);
            return Err(EmbeddingError::Refused {
                status: status.as_u16(),
                message,
            });
        }
        let answer: Answer = response
            .json()
            .await
            .map_err(|e| EmbeddingError::Malformed(e.without_url().to_string()))?;
        if answer.data.len() != texts.len() {
            return Err(EmbeddingError::Malformed(format!(
                "{} vectors for {} texts",
                answer.data.len(),
                texts.len()
            )));
        }
        let mut out = vec![Vec::new(); texts.len()];
        for item in answer.data {
            let slot = out
                .get_mut(item.index)
                .ok_or_else(|| EmbeddingError::Malformed(format!("index {}", item.index)))?;
            *slot = item.embedding;
        }
        let dims = out[0].len();
        if dims == 0 || out.iter().any(|v| v.len() != dims) {
            return Err(EmbeddingError::Malformed(
                "vectors of different lengths".into(),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::{Json, Router, routing::post};

    use super::*;

    #[tokio::test]
    async fn embeds_in_order_through_a_compatible_endpoint() {
        async fn handler(
            headers: axum::http::HeaderMap,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            assert_eq!(headers["authorization"], "Bearer sk-test");
            assert_eq!(body["model"], "tiny");
            let inputs = body["input"].as_array().unwrap();
            // Answer out of order, as the API allows.
            let data: Vec<_> = inputs
                .iter()
                .enumerate()
                .rev()
                .map(|(i, t)| {
                    let len = t.as_str().unwrap().len() as f32;
                    serde_json::json!({"index": i, "embedding": [len, 1.0]})
                })
                .collect();
            Json(serde_json::json!({"data": data}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/embeddings", post(handler)),
            )
            .await
            .unwrap();
        });
        let client = OpenAiCompatible::new(
            &Url::parse(&base).unwrap(),
            "tiny",
            Some(Zeroizing::new("sk-test".into())),
            None,
            None,
        )
        .unwrap();
        assert_eq!(client.model_id(), "tiny");
        let v = client.embed(&["a".into(), "abc".into()]).await.unwrap();
        assert_eq!(v, vec![vec![1.0, 1.0], vec![3.0, 1.0]]);
        assert!(client.embed(&[]).await.unwrap().is_empty());
    }
}

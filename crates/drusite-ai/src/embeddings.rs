// SPDX-License-Identifier: AGPL-3.0-or-later

//! Text embeddings: the vectors semantic search compares.
//!
//! Every vector is stored with the ID of the model that made it ([`Embeddings::model_id`]):
//! vectors from different models live in different spaces and can't be compared, so changing
//! the model means embedding everything again.

use reqwest::Method;
use serde_json::{Value, json};

use crate::error::AiError;
use crate::provider::{Kind, Provider};

/// Something that turns texts into vectors.
pub trait Embeddings: Send + Sync {
    /// Names the vector space: the same ID always means comparable vectors.
    fn model_id(&self) -> &str;

    /// One vector per text, in order.
    fn embed(
        &self,
        texts: &[String],
    ) -> impl std::future::Future<Output = Result<Vec<Vec<f32>>, AiError>> + Send;
}

impl Provider {
    /// One vector per text, in order. `dimensions` asks models that can shorten their vectors
    /// for that many.
    pub async fn embed(
        &self,
        model: &str,
        texts: &[String],
        dimensions: Option<u32>,
    ) -> Result<Vec<Vec<f32>>, AiError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if model.trim().is_empty() {
            return Err(AiError::Invalid("pick an embedding model".to_owned()));
        }
        let vectors = match self.kind() {
            Kind::Anthropic => {
                return Err(AiError::Unsupported(
                    "Anthropic doesn't make embeddings; pick another provider for them".to_owned(),
                ));
            }
            Kind::Google => {
                let model = model.strip_prefix("models/").unwrap_or(model);
                let requests: Vec<_> = texts
                    .iter()
                    .map(|t| {
                        let mut r = json!({
                            "model": format!("models/{model}"),
                            "content": { "parts": [{ "text": t }] },
                        });
                        if let Some(d) = dimensions {
                            r["outputDimensionality"] = json!(d);
                        }
                        r
                    })
                    .collect();
                let url = self.url(&format!("models/{model}:batchEmbedContents"))?;
                let v = self
                    .json(
                        self.request(Method::POST, url)
                            .json(&json!({ "requests": requests })),
                    )
                    .await?;
                v["embeddings"]
                    .as_array()
                    .ok_or_else(|| AiError::Malformed("no “embeddings” list".to_owned()))?
                    .iter()
                    .map(|e| floats(&e["values"]))
                    .collect::<Result<Vec<_>, _>>()?
            }
            Kind::OpenAi | Kind::Mistral | Kind::OpenRouter | Kind::OpenAiCompatible => {
                let mut body = json!({ "model": model, "input": texts });
                if let Some(d) = dimensions {
                    // Mistral calls it differently.
                    let field = if self.kind() == Kind::Mistral {
                        "output_dimension"
                    } else {
                        "dimensions"
                    };
                    body[field] = json!(d);
                }
                let v = self
                    .json(
                        self.request(Method::POST, self.url("embeddings")?)
                            .json(&body),
                    )
                    .await?;
                let data = v["data"]
                    .as_array()
                    .ok_or_else(|| AiError::Malformed("no “data” list".to_owned()))?;
                let mut out = vec![None; data.len()];
                for (n, item) in data.iter().enumerate() {
                    // The API allows any order; a few servers leave the index out.
                    let i = item["index"]
                        .as_u64()
                        .and_then(|i| usize::try_from(i).ok())
                        .unwrap_or(n);
                    let slot = out
                        .get_mut(i)
                        .ok_or_else(|| AiError::Malformed(format!("index {i}")))?;
                    *slot = Some(floats(&item["embedding"])?);
                }
                out.into_iter()
                    .map(|v| v.ok_or_else(|| AiError::Malformed("a vector is missing".to_owned())))
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        if vectors.len() != texts.len() {
            return Err(AiError::Malformed(format!(
                "{} vectors for {} texts",
                vectors.len(),
                texts.len()
            )));
        }
        let dims = vectors[0].len();
        if dims == 0 || vectors.iter().any(|v| v.len() != dims) {
            return Err(AiError::Malformed("vectors of different lengths".into()));
        }
        Ok(vectors)
    }
}

fn floats(v: &Value) -> Result<Vec<f32>, AiError> {
    v.as_array()
        .ok_or_else(|| {
            AiError::Malformed(
                "a vector isn't a list of numbers (base64 vectors aren't supported)".to_owned(),
            )
        })?
        .iter()
        .map(|x| {
            #[allow(clippy::cast_possible_truncation)]
            x.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| AiError::Malformed("a vector holds something not a number".into()))
        })
        .collect()
}

/// An embedding model on a provider: what search indexes with.
#[derive(Debug, Clone)]
pub struct EmbeddingModel {
    provider: Provider,
    model: String,
    id: String,
    dimensions: Option<u32>,
}

impl EmbeddingModel {
    /// `id` names the vector space; by default it's the model name.
    pub fn new(
        provider: Provider,
        model: impl Into<String>,
        id: Option<String>,
        dimensions: Option<u32>,
    ) -> Result<Self, AiError> {
        if !provider.kind().embeds() {
            return Err(AiError::Unsupported(format!(
                "{} doesn't make embeddings",
                provider.kind().label()
            )));
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(AiError::Invalid("pick an embedding model".to_owned()));
        }
        Ok(Self {
            id: id.unwrap_or_else(|| model.clone()),
            provider,
            model,
            dimensions,
        })
    }
}

impl EmbeddingModel {
    /// The provider it's on, for recording what was sent where.
    pub fn provider(&self) -> &Provider {
        &self.provider
    }
}

impl Embeddings for EmbeddingModel {
    fn model_id(&self) -> &str {
        &self.id
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        self.provider
            .embed(&self.model, texts, self.dimensions)
            .await
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider: one vendor account or one self-hosted endpoint, and how to talk to it.

use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::error::AiError;

/// How long a call that isn't a streamed chat may take.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Which API a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    Google,
    Mistral,
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Any server speaking the OpenAI-compatible API: LiteLLM, Ollama, vLLM, SGLang, LM Studio,
    /// llama.cpp and others.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

impl Kind {
    pub const ALL: [Self; 6] = [
        Self::Anthropic,
        Self::OpenAi,
        Self::Google,
        Self::Mistral,
        Self::OpenRouter,
        Self::OpenAiCompatible,
    ];

    /// The name people know it by.
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Google => "Google Gemini",
            Self::Mistral => "Mistral",
            Self::OpenRouter => "OpenRouter",
            Self::OpenAiCompatible => "OpenAI-compatible",
        }
    }

    /// The API's address, for vendors; a custom endpoint has to be given one.
    pub fn default_base(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1"),
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Google => Some("https://generativelanguage.googleapis.com/v1beta"),
            Self::Mistral => Some("https://api.mistral.ai/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::OpenAiCompatible => None,
        }
    }

    /// Vendors need a key; a self-hosted endpoint may not.
    pub fn needs_key(self) -> bool {
        self != Self::OpenAiCompatible
    }

    /// Whether it makes embeddings. Anthropic has no embeddings API.
    pub fn embeds(self) -> bool {
        self != Self::Anthropic
    }

    /// Which wire format its chats use.
    pub(crate) fn dialect(self) -> Dialect {
        match self {
            Self::Anthropic => Dialect::Anthropic,
            Self::Google => Dialect::Google,
            Self::OpenAi | Self::Mistral | Self::OpenRouter | Self::OpenAiCompatible => {
                Dialect::OpenAi
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    OpenAi,
    Anthropic,
    Google,
}

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// What requests name it by.
    pub id: String,
    /// A friendlier name, when the provider gives one.
    pub name: Option<String>,
    /// Whether it looks like an embedding model. Google says so; for the others it's a guess
    /// from the name.
    pub embeddings: bool,
}

/// One configured provider.
#[derive(Clone)]
pub struct Provider {
    pub(crate) http: reqwest::Client,
    kind: Kind,
    base: Url,
    key: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("kind", &self.kind)
            .field("base", &self.base.as_str())
            .field("key", &self.key.as_ref().map(|_| "…"))
            .finish_non_exhaustive()
    }
}

impl Provider {
    /// `base` overrides the vendor's address (a proxy, a regional endpoint); a custom endpoint
    /// must have one, such as `http://localhost:11434/v1`.
    pub fn new(
        http: reqwest::Client,
        kind: Kind,
        base: Option<&Url>,
        key: Option<Zeroizing<String>>,
    ) -> Result<Self, AiError> {
        let base = match (base, kind.default_base()) {
            (Some(b), _) => b.clone(),
            (None, Some(d)) => Url::parse(d).map_err(|e| AiError::Invalid(e.to_string()))?,
            (None, None) => {
                return Err(AiError::Invalid(
                    "an OpenAI-compatible endpoint needs its address, such as http://localhost:11434/v1"
                        .to_owned(),
                ));
            }
        };
        if !matches!(base.scheme(), "http" | "https") {
            return Err(AiError::Invalid(
                "the address has to start with http:// or https://".to_owned(),
            ));
        }
        let key = key.filter(|k| !k.trim().is_empty());
        if kind.needs_key() && key.is_none() {
            return Err(AiError::Invalid(format!(
                "{} needs an API key",
                kind.label()
            )));
        }
        Ok(Self {
            http,
            kind,
            base,
            key,
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    /// `path` under the base address.
    pub(crate) fn url(&self, path: &str) -> Result<Url, AiError> {
        let mut base = self.base.clone();
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        base.join(path).map_err(|e| AiError::Invalid(e.to_string()))
    }

    /// A request with the provider's way of sending the key.
    pub(crate) fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let mut request = self.http.request(method, url);
        let key = self.key.as_ref().map(|k| k.trim().to_owned());
        match self.kind {
            Kind::Anthropic => {
                request = request.header("anthropic-version", "2023-06-01");
                if let Some(k) = key {
                    request = request.header("x-api-key", k);
                }
            }
            Kind::Google => {
                if let Some(k) = key {
                    request = request.header("x-goog-api-key", k);
                }
            }
            Kind::OpenRouter => {
                // OpenRouter lists apps by this name; nothing about the person is sent.
                request = request.header("X-Title", "Drusite");
                if let Some(k) = key {
                    request = request.bearer_auth(k);
                }
            }
            Kind::OpenAi | Kind::Mistral | Kind::OpenAiCompatible => {
                if let Some(k) = key {
                    request = request.bearer_auth(k);
                }
            }
        }
        request
    }

    /// Sends `request`; the answer's body on success, the provider's reason otherwise.
    pub(crate) async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response, AiError> {
        let response = request.send().await.map_err(|e| AiError::network(&e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(AiError::from_status(status.as_u16(), &body))
    }

    pub(crate) async fn json(&self, request: RequestBuilder) -> Result<serde_json::Value, AiError> {
        let response = self.send(request.timeout(CALL_TIMEOUT)).await?;
        let text = response.text().await.map_err(|e| AiError::network(&e))?;
        serde_json::from_str(&text).map_err(|_| {
            AiError::Malformed(format!(
                "expected JSON, got “{}”",
                text.chars().take(80).collect::<String>()
            ))
        })
    }

    /// The models it offers, sorted by ID.
    pub async fn models(&self) -> Result<Vec<Model>, AiError> {
        let mut models = match self.kind {
            Kind::Anthropic => {
                let mut url = self.url("models")?;
                url.query_pairs_mut().append_pair("limit", "1000");
                let v = self.json(self.request(Method::GET, url)).await?;
                list(&v["data"], "data")?
                    .iter()
                    .filter_map(|m| {
                        Some(Model {
                            id: m["id"].as_str()?.to_owned(),
                            name: m["display_name"].as_str().map(str::to_owned),
                            embeddings: false,
                        })
                    })
                    .collect()
            }
            Kind::Google => self.google_models().await?,
            Kind::OpenAi | Kind::Mistral | Kind::OpenRouter | Kind::OpenAiCompatible => {
                let v = self
                    .json(self.request(Method::GET, self.url("models")?))
                    .await?;
                list(&v["data"], "data")?
                    .iter()
                    .filter_map(|m| {
                        let id = m["id"].as_str()?.to_owned();
                        Some(Model {
                            embeddings: looks_like_embeddings(&id),
                            name: m["name"].as_str().map(str::to_owned),
                            id,
                        })
                    })
                    .collect()
            }
        };
        models.sort_by(|a: &Model, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        Ok(models)
    }

    async fn google_models(&self) -> Result<Vec<Model>, AiError> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        // A few pages at most; Google lists fewer than a hundred models.
        for _ in 0..5 {
            let mut url = self.url("models")?;
            url.query_pairs_mut().append_pair("pageSize", "1000");
            if let Some(p) = &page {
                url.query_pairs_mut().append_pair("pageToken", p);
            }
            let v = self.json(self.request(Method::GET, url)).await?;
            for m in list(&v["models"], "models")? {
                let Some(name) = m["name"].as_str() else {
                    continue;
                };
                let methods: Vec<&str> = m["supportedGenerationMethods"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                let embeddings = methods.iter().any(|x| x.contains("mbed"));
                if !embeddings && !methods.contains(&"generateContent") && !methods.is_empty() {
                    continue;
                }
                out.push(Model {
                    id: name.strip_prefix("models/").unwrap_or(name).to_owned(),
                    name: m["displayName"].as_str().map(str::to_owned),
                    embeddings,
                });
            }
            match v["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => page = Some(t.to_owned()),
                _ => break,
            }
        }
        Ok(out)
    }
}

fn list<'a>(v: &'a serde_json::Value, field: &str) -> Result<&'a Vec<serde_json::Value>, AiError> {
    v.as_array()
        .ok_or_else(|| AiError::Malformed(format!("no “{field}” list of models")))
}

fn looks_like_embeddings(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.contains("embed") || id.contains("bge-") || id.contains("e5-") || id.contains("minilm")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendors_have_addresses_and_custom_endpoints_need_one() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::new();
        let p = Provider::new(
            http.clone(),
            Kind::Mistral,
            None,
            Some(Zeroizing::new("k".into())),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            p.url("chat/completions").map(|u| u.to_string()),
            Ok("https://api.mistral.ai/v1/chat/completions".to_owned())
        );
        assert!(matches!(
            Provider::new(http.clone(), Kind::OpenAiCompatible, None, None),
            Err(AiError::Invalid(_))
        ));
        assert!(matches!(
            Provider::new(
                http.clone(),
                Kind::OpenAi,
                None,
                Some(Zeroizing::new(" ".into()))
            ),
            Err(AiError::Invalid(_))
        ));
        let local = Url::parse("http://localhost:11434/v1/").unwrap_or_else(|e| panic!("{e}"));
        let p = Provider::new(http, Kind::OpenAiCompatible, Some(&local), None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            p.url("models").map(|u| u.to_string()),
            Ok("http://localhost:11434/v1/models".to_owned())
        );
        assert_eq!(
            serde_json::to_string(&Kind::OpenAiCompatible).ok(),
            Some("\"openai-compatible\"".to_owned())
        );
    }
}

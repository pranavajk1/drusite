// SPDX-License-Identifier: AGPL-3.0-or-later

//! AI providers on this device: which ones, their keys, and which provider and model each
//! feature uses.
//!
//! Keys are kept in the local store, which is encrypted with a key from the OS key store and
//! never synced: a provider set up on the Mac isn't on the phone until it's set up there too. The
//! UI never gets a key back, only its last four characters.
//!
//! Saving and choosing are ordinary commands ([`Core::handle`]); listing models and *Test
//! connection* reach the network, so they run through [`run`], which holds the core only long
//! enough to read the provider.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use drusite_ai::{Kind, Provider};
use url::Url;
use zeroize::Zeroizing;

use crate::api::{
    AiCheckStep, AiChoice, AiFeature, AiFeatureChoice, AiKind, AiKindInfo, AiModel, AiProvider,
    ApiError, Command, ErrorCode, Event, Response,
};
use crate::engine::Core;
use crate::store::AiProviderRow;

pub type SharedCore = Arc<Mutex<Core>>;

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Invalid, message.into())
}

pub(crate) fn kind_of(kind: AiKind) -> Kind {
    match kind {
        AiKind::Anthropic => Kind::Anthropic,
        AiKind::OpenAi => Kind::OpenAi,
        AiKind::Google => Kind::Google,
        AiKind::Mistral => Kind::Mistral,
        AiKind::OpenRouter => Kind::OpenRouter,
        AiKind::OpenAiCompatible => Kind::OpenAiCompatible,
    }
}

fn api_kind(kind: Kind) -> AiKind {
    match kind {
        Kind::Anthropic => AiKind::Anthropic,
        Kind::OpenAi => AiKind::OpenAi,
        Kind::Google => AiKind::Google,
        Kind::Mistral => AiKind::Mistral,
        Kind::OpenRouter => AiKind::OpenRouter,
        Kind::OpenAiCompatible => AiKind::OpenAiCompatible,
    }
}

/// A kind as the store keeps it: its config name, as in the server's config.
fn kind_name(kind: Kind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn kind_from_name(name: &str) -> Option<Kind> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

fn key_hint(key: Option<&Zeroizing<String>>) -> Option<String> {
    let key = key?.trim();
    if key.is_empty() {
        return None;
    }
    let chars: Vec<char> = key.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    Some(format!("…{tail}"))
}

fn provider_view(row: &AiProviderRow) -> Option<AiProvider> {
    Some(AiProvider {
        id: row.id.clone(),
        kind: api_kind(kind_from_name(&row.kind)?),
        name: row.name.clone(),
        base_url: row.base_url.clone(),
        key_hint: key_hint(row.api_key.as_ref()),
    })
}

fn feature_from_name(name: &str) -> Option<AiFeature> {
    AiFeature::ALL.into_iter().find(|f| f.name() == name)
}

/// The HTTP client providers are reached with. On Android the system's certificate checker
/// needs the app's Java side, so Mozilla's roots are built in there; elsewhere the system's
/// own trust store is used, so a home lab's own certificate authority works.
fn http() -> Result<reqwest::Client, ApiError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120));
    #[cfg(target_os = "android")]
    let builder = {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        builder.tls_backend_preconfigured(tls)
    };
    builder
        .build()
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

impl Core {
    pub(crate) fn ai(&self) -> Result<Response, ApiError> {
        let providers = self
            .store
            .ai_providers()?
            .iter()
            .filter_map(provider_view)
            .collect();
        let saved = self.store.ai_features()?;
        let features = AiFeature::ALL
            .into_iter()
            .map(|feature| AiFeatureChoice {
                feature,
                choice: saved.iter().find(|(f, _, _)| f == feature.name()).map(
                    |(_, provider, model)| AiChoice {
                        provider: provider.clone(),
                        model: model.clone(),
                    },
                ),
            })
            .collect();
        let kinds = Kind::ALL
            .into_iter()
            .map(|k| AiKindInfo {
                kind: api_kind(k),
                label: k.label().to_owned(),
                default_base_url: k.default_base().map(str::to_owned),
                needs_key: k.needs_key(),
                embeds: k.embeds(),
            })
            .collect();
        Ok(Response::Ai {
            available: true,
            kinds,
            providers,
            features,
        })
    }

    pub(crate) fn save_ai_provider(
        &mut self,
        id: Option<String>,
        kind: AiKind,
        name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<Response, ApiError> {
        let kind = kind_of(kind);
        let name = name.trim();
        let name = if name.is_empty() { kind.label() } else { name };
        if name.chars().count() > 80 {
            return Err(bad("a provider's name is at most 80 characters"));
        }
        let base_url = base_url
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty());
        if let Some(u) = &base_url {
            let parsed =
                Url::parse(u).map_err(|_| bad(format!("“{u}” isn't an address (http://…)")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(bad("the address has to start with http:// or https://"));
            }
        }
        let existing = match &id {
            Some(id) => Some(
                self.store
                    .ai_provider(id)?
                    .ok_or_else(|| bad("no such provider"))?,
            ),
            None => None,
        };
        let api_key = match api_key {
            // Unchanged.
            None => existing.as_ref().and_then(|r| r.api_key.clone()),
            Some(k) if k.trim().is_empty() => None,
            Some(k) => Some(Zeroizing::new(k.trim().to_owned())),
        };
        // The same checks as connecting would make, so a broken provider isn't saved.
        Provider::new(
            http()?,
            kind,
            base_url
                .as_deref()
                .and_then(|u| Url::parse(u).ok())
                .as_ref(),
            api_key.clone(),
        )
        .map_err(|e| bad(e.to_string()))?;
        let row = AiProviderRow {
            id: id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            kind: kind_name(kind),
            name: name.to_owned(),
            base_url,
            api_key,
            created_at: existing
                .as_ref()
                .map_or_else(crate::trash::now_ms, |r| r.created_at),
        };
        self.store.put_ai_provider(&row)?;
        // A provider that no longer embeds can't keep the embeddings feature.
        if !kind.embeds() {
            let uses = self.store.ai_features()?;
            if uses
                .iter()
                .any(|(f, p, _)| f == AiFeature::Embeddings.name() && *p == row.id)
            {
                self.store
                    .set_ai_feature(AiFeature::Embeddings.name(), None)?;
            }
        }
        (self.events)(Event::AiChanged);
        Ok(Response::AiProviderSaved {
            provider: provider_view(&row).ok_or_else(|| bad("unknown kind"))?,
        })
    }

    pub(crate) fn delete_ai_provider(&mut self, id: &str) -> Result<(), ApiError> {
        if !self.store.remove_ai_provider(id)? {
            return Err(bad("no such provider"));
        }
        (self.events)(Event::AiChanged);
        Ok(())
    }

    pub(crate) fn set_ai_feature(
        &mut self,
        feature: AiFeature,
        choice: Option<AiChoice>,
    ) -> Result<(), ApiError> {
        if let Some(c) = &choice {
            let row = self
                .store
                .ai_provider(&c.provider)?
                .ok_or_else(|| bad("no such provider"))?;
            let kind = kind_from_name(&row.kind).ok_or_else(|| bad("unknown kind"))?;
            if feature == AiFeature::Embeddings && !kind.embeds() {
                return Err(bad(format!("{} doesn't make embeddings", kind.label())));
            }
            if c.model.trim().is_empty() {
                return Err(bad("pick a model"));
            }
        }
        self.store.set_ai_feature(
            feature.name(),
            choice
                .as_ref()
                .map(|c| (c.provider.as_str(), c.model.trim())),
        )?;
        (self.events)(Event::AiChanged);
        Ok(())
    }

    /// The provider with this ID, ready to call.
    pub fn ai_provider(&self, id: &str) -> Result<Provider, ApiError> {
        let row = self
            .store
            .ai_provider(id)?
            .ok_or_else(|| bad("no such provider"))?;
        let kind = kind_from_name(&row.kind).ok_or_else(|| bad("unknown kind"))?;
        let base = row
            .base_url
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|e| bad(e.to_string()))?;
        Provider::new(http()?, kind, base.as_ref(), row.api_key.clone())
            .map_err(|e| bad(e.to_string()))
    }

    /// The provider and model a feature uses, if one is picked.
    pub fn ai_for(&self, feature: AiFeature) -> Result<Option<(Provider, String)>, ApiError> {
        let Some((_, provider, model)) = self
            .store
            .ai_features()?
            .into_iter()
            .find(|(f, _, _)| feature_from_name(f) == Some(feature))
        else {
            return Ok(None);
        };
        Ok(Some((self.ai_provider(&provider)?, model)))
    }
}

/// Whether [`run`] answers this command (it reaches the network).
pub fn is_network(command: &Command) -> bool {
    matches!(
        command,
        Command::AiModels { .. } | Command::TestAiProvider { .. }
    )
}

/// Answers the commands that reach a provider, without holding the core while waiting.
pub async fn run(core: &SharedCore, command: Command) -> Result<Response, ApiError> {
    let provider_of = |id: &str| -> Result<Provider, ApiError> {
        core.lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "the core crashed earlier"))?
            .ai_provider(id)
    };
    match command {
        Command::AiModels { provider } => {
            let provider = provider_of(&provider)?;
            let models = provider
                .models()
                .await
                .map_err(|e| ApiError::new(ErrorCode::Account, e.to_string()))?;
            Ok(Response::AiModels {
                models: models
                    .into_iter()
                    .map(|m| AiModel {
                        id: m.id,
                        name: m.name,
                        embeddings: m.embeddings,
                    })
                    .collect(),
            })
        }
        Command::TestAiProvider {
            provider,
            chat_model,
            embedding_model,
        } => {
            let provider = provider_of(&provider)?;
            let embedding_model = embedding_model.filter(|_| provider.kind().embeds());
            let steps =
                drusite_ai::check(&provider, chat_model.as_deref(), embedding_model.as_deref())
                    .await;
            Ok(Response::AiCheck {
                steps: steps
                    .into_iter()
                    .map(|s| AiCheckStep {
                        what: s.what,
                        ok: s.ok,
                        detail: s.detail,
                        millis: u32::try_from(s.millis).unwrap_or(u32::MAX),
                    })
                    .collect(),
            })
        }
        _ => Err(ApiError::new(
            ErrorCode::Internal,
            "not a command that reaches a provider",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in Kind::ALL {
            assert_eq!(kind_from_name(&kind_name(kind)), Some(kind));
            assert_eq!(kind_of(api_kind(kind)), kind);
        }
        assert_eq!(kind_name(Kind::OpenAiCompatible), "openai-compatible");
        assert_eq!(
            key_hint(Some(&Zeroizing::new("sk-abcdef".into()))).as_deref(),
            Some("…cdef")
        );
        assert_eq!(
            key_hint(Some(&Zeroizing::new("ab".into()))).as_deref(),
            Some("…ab")
        );
        assert_eq!(key_hint(None), None);
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! The AI provider interface and its adapters, shared by clients and the server.
//!
//! One [`Provider`] per configured account or endpoint, of one [`Kind`]: a native adapter per
//! vendor (Anthropic, OpenAI, Google, Mistral, OpenRouter), or a custom endpoint speaking the
//! OpenAI-compatible API (LiteLLM, Ollama, vLLM, SGLang, LM Studio, llama.cpp and the like).
//! Native adapters exist because "OpenAI-compatible" vendors differ in the details: where the key
//! goes, what the token limit is called, how a stream ends, what an error looks like.
//!
//! A provider streams chats ([`Provider::chat`]), makes embeddings ([`Provider::embed`]) and
//! lists its models ([`Provider::models`]); [`check`] is the *Test connection* check. Features
//! (chat, embeddings, later transcription) each pick their own provider and model.

pub mod chat;
pub mod check;
pub mod embeddings;
mod error;
pub mod provider;
mod sse;

pub use chat::{ChatEvent, ChatRequest, ChatStream, Message, Reply, Role, Usage};
pub use check::{CheckStep, check};
pub use embeddings::{EmbeddingModel, Embeddings};
pub use error::AiError;
pub use provider::{Kind, Model, Provider};

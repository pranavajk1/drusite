// SPDX-License-Identifier: AGPL-3.0-or-later

//! The AI provider interface and its adapters, shared by clients and the server.
//!
//! For now, embeddings through any OpenAI-compatible endpoint (OpenAI itself, LiteLLM, Ollama,
//! vLLM and the like), which search needs. The full provider interface, with native adapters
//! per vendor, chat and a connection test, is built on top of it.

pub mod embeddings;

pub use embeddings::{EmbeddingError, Embeddings, OpenAiCompatible};

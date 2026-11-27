// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Drusite server: one binary with its modules inside. It needs PostgreSQL 16+ with
//! pgvector, and any S3-compatible store; nothing else.

pub mod account;
pub mod app;
pub mod ask;
pub mod attachments;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod health;
pub mod keys;
pub mod mcp;
pub mod policy;
pub mod search;
pub mod storage;
pub mod sync;
pub mod telemetry;

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Databases, read: what a view shows ([`compute`]) and rows as Markdown ([`markdown`]).
//!
//! Shared by the core (the app's views, the Markdown folder) and the agents' tools (on the
//! server and on the desktop), so a view, a formula or a row's file reads the same everywhere.

pub mod compute;
pub mod markdown;

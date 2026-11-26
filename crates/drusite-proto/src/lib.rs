// SPDX-License-Identifier: AGPL-3.0-or-later

//! The sync protocol: versioned Protobuf over WebSocket, so old and new clients keep talking.
//!
//! # Connection
//!
//! A device opens a WebSocket to `/api/v1/sync` with the subprotocol [`SUBPROTOCOL`] and its
//! DPoP-bound access token (one proof per connection). The server answers with [`Welcome`].
//! Every message after that is one binary WebSocket message: a [`Frame`].
//!
//! # Documents
//!
//! Each workspace document and page document is synced on its own subscription:
//!
//! 1. The device sends [`Subscribe`] with the highest server sequence number it has imported for
//!    that document (0 if none). For a document the device made and the server has never seen,
//!    it carries the document's first snapshot, so a document is created exactly once.
//! 2. The server sends everything after that sequence number in [`Updates`] (a snapshot first, if
//!    the log has been compacted past it), then [`Subscribed`].
//! 3. From then on the device sends its changes as [`Push`]; the server stores each, answers
//!    [`Ack`] with its sequence number, and sends it to the document's other subscribers as
//!    [`Updates`].
//!
//! Loro ignores updates it already has, so sending something twice is always safe: a device that
//! loses its connection before an `Ack` simply pushes again.
//!
//! # Compatibility
//!
//! Fields are only ever added, with new tags. Old peers skip fields they don't know. A change that
//! can't be made that way is a new [`PROTOCOL_VERSION`] and a new subprotocol.

pub mod frame;
mod messages;

pub use frame::{FrameError, decode, encode};
pub use messages::*;

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// The WebSocket subprotocol for [`PROTOCOL_VERSION`].
pub const SUBPROTOCOL: &str = "drusite.sync.v1";

/// The largest frame either side accepts, compressed or not.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

// SPDX-License-Identifier: AGPL-3.0-or-later

//! The messages. Written with prost's derives rather than generated, so building needs no
//! `protoc`; the tags below are the wire format and never change.

/// What kind of document a subscription is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum DocKind {
    Unspecified = 0,
    /// The workspace document: the page tree.
    Workspace = 1,
    Page = 2,
}

/// Anything a device sends.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClientMessage {
    #[prost(oneof = "client_message::Body", tags = "1, 2, 3, 4")]
    pub body: Option<client_message::Body>,
}

pub mod client_message {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Subscribe(super::Subscribe),
        #[prost(message, tag = "2")]
        Unsubscribe(super::Unsubscribe),
        #[prost(message, tag = "3")]
        Push(super::Push),
        #[prost(message, tag = "4")]
        Ping(super::Ping),
    }
}

/// Anything the server sends.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ServerMessage {
    #[prost(oneof = "server_message::Body", tags = "1, 2, 3, 4, 5, 6")]
    pub body: Option<server_message::Body>,
}

pub mod server_message {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Welcome(super::Welcome),
        #[prost(message, tag = "2")]
        Updates(super::Updates),
        #[prost(message, tag = "3")]
        Subscribed(super::Subscribed),
        #[prost(message, tag = "4")]
        Ack(super::Ack),
        #[prost(message, tag = "5")]
        Error(super::Error),
        #[prost(message, tag = "6")]
        Pong(super::Pong),
    }
}

/// The server's first message.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Welcome {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// The server's version, for logs.
    #[prost(string, tag = "2")]
    pub server_version: String,
    #[prost(uint32, tag = "3")]
    pub max_frame_bytes: u32,
}

/// Start syncing a document.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Subscribe {
    /// The document's ID: 16 bytes, a UUID.
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
    /// The workspace it belongs to (for the workspace document, its own ID).
    #[prost(bytes = "vec", tag = "2")]
    pub workspace: Vec<u8>,
    #[prost(enumeration = "DocKind", tag = "3")]
    pub kind: i32,
    /// The highest server sequence number the device has imported; 0 for none.
    #[prost(uint64, tag = "4")]
    pub after_seq: u64,
    /// The document's first snapshot, for a document the server may not have yet. Ignored if it
    /// already exists.
    #[prost(bytes = "vec", tag = "5")]
    pub create: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Unsubscribe {
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
}

/// A change to a subscribed document: a Loro update.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Push {
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
    /// Chosen by the device, echoed in the [`Ack`] or [`Error`].
    #[prost(uint64, tag = "2")]
    pub push_id: u64,
    #[prost(bytes = "vec", tag = "3")]
    pub update: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Ping {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Pong {
    #[prost(uint64, tag = "1")]
    pub nonce: u64,
}

/// One stored entry of a document's log.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Entry {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    /// A Loro update, or a snapshot covering everything up to `seq`.
    #[prost(bytes = "vec", tag = "2")]
    pub data: Vec<u8>,
    #[prost(bool, tag = "3")]
    pub snapshot: bool,
}

/// Entries of a document the device doesn't have yet, in order.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Updates {
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
    #[prost(message, repeated, tag = "2")]
    pub entries: Vec<Entry>,
}

/// The catch-up for a [`Subscribe`] is complete; live updates follow.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Subscribed {
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
    /// The server's newest sequence number for the document.
    #[prost(uint64, tag = "2")]
    pub head_seq: u64,
    /// The server made the document from this subscription's snapshot.
    #[prost(bool, tag = "3")]
    pub created: bool,
    /// The device may push; otherwise it can only read.
    #[prost(bool, tag = "4")]
    pub writable: bool,
}

/// A [`Push`] is stored.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Ack {
    #[prost(bytes = "vec", tag = "1")]
    pub doc: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub push_id: u64,
    #[prost(uint64, tag = "3")]
    pub seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ErrorCode {
    Unspecified = 0,
    /// The message couldn't be understood.
    BadRequest = 1,
    /// Not allowed to read, write or create the document.
    Forbidden = 2,
    /// The document doesn't exist, and the subscription didn't create it.
    NotFound = 3,
    /// The update isn't a valid Loro update.
    InvalidUpdate = 4,
    /// Too big.
    TooLarge = 5,
    /// A push for a document that isn't subscribed.
    NotSubscribed = 6,
    /// Something went wrong on the server; try again later.
    Internal = 7,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Error {
    #[prost(enumeration = "ErrorCode", tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
    /// The document it's about, if any.
    #[prost(bytes = "vec", tag = "3")]
    pub doc: Vec<u8>,
    /// The push it's about, if any.
    #[prost(uint64, tag = "4")]
    pub push_id: u64,
}

impl ClientMessage {
    pub fn new(body: client_message::Body) -> Self {
        Self { body: Some(body) }
    }
}

impl ServerMessage {
    pub fn new(body: server_message::Body) -> Self {
        Self { body: Some(body) }
    }
}

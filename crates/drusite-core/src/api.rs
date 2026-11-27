// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Core API: the commands the UI sends, the responses it gets back, and the events the core
//! pushes to it.
//!
//! This is the UI's only way into the core. Each platform carries it its own way (Tauri's IPC
//! in `drusite-app`, direct calls in `drusite-wasm`), and the UI never knows which. The
//! TypeScript types are generated from these definitions (`cargo xtask bindings`), so the two
//! sides can't drift.
//!
//! Positions are UTF-16 code units within one block's text, as in the browser.

use drusite_model::database::{PropertyDef, Schema, Value, View};
pub use drusite_model::search::SnippetPart;
use drusite_model::{
    Block, BlockId, BlockKind, Day, Mark, PageId, PageKind, PageQuery, Property, PropertyValue,
};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Something the UI asks the core to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    /// The page tree. Answered with [`Response::Pages`].
    ListPages,
    /// Answered with [`Response::PageCreated`].
    CreatePage {
        parent: Option<PageId>,
        title: String,
    },
    RenamePage {
        page: PageId,
        title: String,
    },
    /// Moves a page to position `index` among `parent`'s children (or the top-level pages).
    MovePage {
        page: PageId,
        parent: Option<PageId>,
        index: u32,
    },
    /// Moves a page, with its subpages, to the trash. Answered with [`Response::Done`].
    DeletePage {
        page: PageId,
    },
    /// What's in the trash. Answered with [`Response::Trash`].
    Trash,
    /// Puts a page back from the trash where it was (at the top level if the page it was under
    /// is still in the trash).
    RestorePage {
        page: PageId,
    },
    /// Deletes a page in the trash, and its subpages, for good. Refused for a page that isn't
    /// in the trash.
    DeleteForever {
        page: PageId,
    },
    /// Deletes everything in the trash for good.
    EmptyTrash,
    /// Keeps a file on this device, to put on a page; it goes to the server when the device
    /// syncs. `data` is the file, in base64. Answered with [`Response::Attachment`], whose kind
    /// is the block to insert.
    AddAttachment {
        name: String,
        mime: String,
        data: String,
    },
    /// A file a page's attachment block names. Answered with [`Response::AttachmentData`], or
    /// [`ErrorCode::NotHere`] while it's being fetched from the server; then
    /// [`Event::AttachmentArrived`] says when it's here.
    ReadAttachment {
        blob: String,
    },
    /// The Markdown bridge: its folder and how its last round went. Answered with
    /// [`Response::Bridge`]; [`ErrorCode::Unsupported`] where there is no bridge (phones, the
    /// browser).
    BridgeStatus,
    /// Turns the Markdown bridge on with `folder` (a full path) or off. Answered with
    /// [`Response::Bridge`] once the first round has run.
    SetBridgeFolder {
        folder: Option<String>,
    },
    /// Imports a folder of Markdown notes (an Obsidian vault, say) as new pages under a new
    /// top-level page; the folder isn't changed. Answered with [`Response::Imported`]; desktops
    /// only.
    ImportFolder {
        folder: String,
    },
    /// Loads a page for editing. Answered with [`Response::Opened`]. Each open is matched by a
    /// [`Command::ClosePage`]; the page stays open until every view that opened it has closed it.
    OpenPage {
        page: PageId,
    },
    ClosePage {
        page: PageId,
    },
    /// Applies edits to an open page, in order, and saves them. Answered with
    /// [`Response::Edited`].
    Edit {
        page: PageId,
        edits: Vec<Edit>,
    },
    /// The UI has loaded and is showing what it opened. Benchmarks wait for this.
    Ready,
    /// How this device stands with its server: signed in or not, and how sync is going.
    AccountStatus,
    /// Signs in to the Drusite server at `server` (its address, as the person typed it), in the
    /// system browser. Answers at once; how it goes arrives as [`Event::AccountChanged`].
    /// Only the apps can sign in; elsewhere this is [`ErrorCode::Unsupported`].
    SignIn {
        server: String,
    },
    /// Gives up a sign-in still waiting in the browser.
    CancelSignIn,
    /// Signs this device out of its server. Its notes stay on the device.
    SignOut,
    /// Sets a page's property, or removes it (`value` none).
    SetProperty {
        page: PageId,
        name: String,
        value: Option<PropertyValue>,
    },
    /// The journal entry for `date` (the person's own day, as their device has it), made from
    /// the template if there isn't one yet. Answered with [`Response::JournalDay`].
    OpenJournalDay {
        date: Day,
    },
    /// The pages a query matches. Answered with [`Response::Pages`].
    QueryPages {
        query: PageQuery,
    },
    /// The settings that sync with the account. Answered with [`Response::Settings`].
    Settings,
    /// Changes one setting. Answered with [`Response::Settings`].
    SetSetting {
        setting: Setting,
    },
    /// Full-text search of this device's pages: every word must match, the last as a prefix.
    /// Answered with [`Response::SearchResults`], best first, one hit per page.
    Search {
        query: String,
        limit: u32,
    },
    /// Agents: the local MCP endpoint, the tokens made for agents on this computer, and what
    /// agents did (here and, on the apps, on the server). Answered with [`Response::Agents`].
    Agents,
    /// Turns the local MCP endpoint (`http://127.0.0.1:<port>/mcp`) on or off. Desktops only.
    SetLocalAgents {
        enabled: bool,
        port: u16,
    },
    /// Makes a token for an agent on this computer. Answered with [`Response::AgentToken`]:
    /// the only time the token is shown.
    CreateAgentToken {
        name: String,
        access: AgentAccess,
    },
    /// Disconnects an agent on this computer.
    RevokeAgentToken {
        id: String,
    },
    /// Takes back what one agent call changed, as a new edit.
    UndoAgentCall {
        id: String,
        source: AgentSource,
    },
    /// The AI providers on this device and what each feature uses. Answered with
    /// [`Response::Ai`].
    Ai,
    /// Adds a provider (`id` none) or changes one. A `None` key keeps the one already saved; an
    /// empty one removes it. Answered with [`Response::AiProviderSaved`].
    SaveAiProvider {
        id: Option<String>,
        kind: AiKind,
        name: String,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    /// Removes a provider; the features that used it are left without one.
    DeleteAiProvider {
        id: String,
    },
    /// Picks the provider and model a feature uses, or none.
    SetAiFeature {
        feature: AiFeature,
        choice: Option<AiChoice>,
    },
    /// The models a provider offers. Answered with [`Response::AiModels`].
    AiModels {
        provider: String,
    },
    /// *Test connection*: lists the models, then tries each model given with a tiny request.
    /// Answered with [`Response::AiCheck`].
    TestAiProvider {
        provider: String,
        chat_model: Option<String>,
        embedding_model: Option<String>,
    },
    /// Whether Ask your notes works with this device's server, and with what. Answered with
    /// [`Response::AskStatus`].
    AskStatus,
    /// The saved conversations, most recent first. Answered with [`Response::Conversations`].
    Conversations,
    /// One conversation, with its messages. Answered with [`Response::Conversation`].
    Conversation {
        id: String,
    },
    RenameConversation {
        id: String,
        title: String,
    },
    DeleteConversation {
        id: String,
    },
    /// Asks the notes a question (continuing `conversation`, or starting one). Answered with
    /// [`Response::Done`] at once; the answer arrives as [`Event::Ask`]s naming `request`, which
    /// the UI picks.
    Ask {
        request: String,
        conversation: Option<String>,
        question: String,
        /// The local date, `YYYY-MM-DD`, so "yesterday" means the asker's.
        today: Option<String>,
    },
    /// Stops following an answer. The server still finishes and saves it.
    CancelAsk {
        request: String,
    },
    /// How a page is shared to the web, as the server says (and whether this person may change
    /// it). Answered with [`Response::Share`]; when the server can't be asked, with what the
    /// workspace last heard and `available` false.
    Share {
        page: PageId,
    },
    /// How a page is shared, from the workspace document alone (no server needed). Answered
    /// with [`Response::Share`].
    LocalShare {
        page: PageId,
    },
    /// Publishes a page to the web, or changes how. Answered with [`Response::Share`].
    Publish {
        page: PageId,
        change: ShareChange,
    },
    /// Takes a page off the web at once. Answered with [`Response::Share`].
    Unpublish {
        page: PageId,
    },
    /// Gives a published page a new link; the old one stops working. Answered with
    /// [`Response::Share`].
    RegenerateShareLink {
        page: PageId,
    },
    /// Makes a database under `parent` (inside it, shown as a block, when `inline`). Answered
    /// with [`Response::DatabaseCreated`].
    CreateDatabase {
        parent: Option<PageId>,
        title: String,
        inline: bool,
        template: crate::database::Template,
    },
    /// A database's schema and views. Answered with [`Response::Database`].
    Database {
        database: PageId,
    },
    /// What a view shows (the first view when `view` is none). Answered with
    /// [`Response::View`].
    QueryView {
        database: PageId,
        view: Option<String>,
        clock: crate::database::Clock,
    },
    /// A row's cells, for its page; none when the page isn't a row. Answered with
    /// [`Response::RowCells`].
    RowCells {
        row: PageId,
        clock: crate::database::Clock,
    },
    /// Adds or changes a property. A new relation with `twin_name` is two-way: the related
    /// database gets a property of that name showing the other side. Answered with
    /// [`Response::Database`].
    PutProperty {
        database: PageId,
        property: PropertyDef,
        twin_name: Option<String>,
    },
    /// Removes a property (and a two-way relation's other side). Answered with
    /// [`Response::Database`].
    DeleteProperty {
        database: PageId,
        property: String,
    },
    /// Adds or changes a view. Answered with [`Response::Database`].
    PutView {
        database: PageId,
        view: View,
    },
    DeleteView {
        database: PageId,
        view: String,
    },
    /// Sets a cell, or clears it (`value` none). The title cell renames the row; the far side
    /// of a two-way relation changes the rows on the other end.
    SetCell {
        row: PageId,
        property: String,
        value: Option<Value>,
    },
    /// Adds a row (after `after`, or last) with these values. Answered with
    /// [`Response::PageCreated`].
    CreateRow {
        database: PageId,
        title: String,
        values: Vec<CellValue>,
        after: Option<PageId>,
    },
    /// Whether a formula reads and type-checks, against a database's properties. Answered with
    /// [`Response::FormulaCheck`].
    CheckFormula {
        database: PageId,
        expression: String,
    },
    /// Imports a Notion export (its `.zip`, or the folder it unzips to), offline, as new pages
    /// under a new top-level page. Answered with [`Response::NotionImported`]; desktops.
    ImportNotion {
        path: String,
    },
    /// The same, from the export's bytes (base64), for phones and the browser.
    ImportNotionData {
        name: String,
        data: String,
    },
    /// Imports straight from Notion's API, with an internal integration's token: every page
    /// shared with the integration. A token given is kept in the device's key store for next
    /// time; none uses the one kept. Answered with [`Response::NotionImported`] at the end,
    /// with [`Event::NotionProgress`] on the way.
    ImportNotionApi {
        token: Option<String>,
    },
    /// Whether a Notion token is kept. Answered with [`Response::NotionToken`].
    NotionToken,
    /// Forgets the kept Notion token.
    ForgetNotionToken,
}

/// How a page is shared to the web.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ShareInfo {
    pub published: bool,
    /// The public link, while it's published.
    pub url: Option<String>,
    pub include_subpages: bool,
    pub allow_indexing: bool,
    /// When the link stops working, in Unix milliseconds.
    #[specta(type = Option<u32>)]
    pub expires_at: Option<i64>,
    pub expired: bool,
    pub has_password: bool,
    /// Whether this person may change it.
    pub can_publish: bool,
    /// Whether this came from the server now. When false, it's what the workspace last heard,
    /// and `message` says why the server couldn't be asked.
    pub available: bool,
    pub message: Option<String>,
}

/// A change to how a page is shared. Fields left out stay as they are (off, for a new link).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ShareChange {
    pub include_subpages: Option<bool>,
    pub allow_indexing: Option<bool>,
    pub expiry: Option<ExpiryChange>,
    pub password: Option<PasswordChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExpiryChange {
    /// Unix milliseconds, in the future.
    Set {
        #[specta(type = u32)]
        at: i64,
    },
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PasswordChange {
    Set { password: String },
    Clear,
}

/// One value for [`Command::CreateRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CellValue {
    pub property: String,
    pub value: Value,
}

/// Whether Ask your notes works, as the server says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AskStatus {
    pub signed_in: bool,
    pub available: bool,
    /// Why not, in words: no server, no chat model on it, search off.
    pub message: Option<String>,
    /// `not_signed_in`, `unreachable`, `disabled`, `search_off`, `no_chat_model`, `old_server`.
    pub reason: Option<String>,
    /// The chat model's name.
    pub model: Option<String>,
    /// Whether meaning is searched too (the server has an embedding model).
    pub semantic: bool,
    /// The server's embedding model, when it has one.
    pub embedding_model: Option<String>,
    /// Documents waiting to be indexed.
    pub indexing: u32,
}

/// A saved conversation, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    /// Unix milliseconds.
    #[specta(type = u32)]
    pub updated_at: i64,
}

/// A block an answer cites as `[n]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AskSource {
    pub n: u32,
    pub page: PageId,
    pub block: Option<BlockId>,
    pub title: String,
    /// The page is gone, or Ask may no longer read it.
    pub missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AskRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AskMessage {
    pub id: String,
    pub role: AskRole,
    pub text: String,
    /// The sources it cites (answers only).
    pub sources: Vec<AskSource>,
    pub model: Option<String>,
    /// Why the answer stopped short, if it did.
    pub error: Option<String>,
    /// Unix milliseconds.
    #[specta(type = u32)]
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub messages: Vec<AskMessage>,
}

/// A step in an answer as it streams in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AskUpdate {
    /// The question is saved, in this conversation.
    Started {
        conversation: String,
        title: String,
        message: String,
    },
    /// The blocks the answer may cite, by number.
    Sources { sources: Vec<AskSource> },
    /// More of the answer.
    Text { text: String },
    /// The answer is complete and saved; `cited` are the numbers it cited.
    Done {
        message: Option<String>,
        cited: Vec<u32>,
        model: Option<String>,
    },
    /// It stopped; what came before is saved.
    Failed { message: String },
}

/// Which API an AI provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AiKind {
    Anthropic,
    OpenAi,
    Google,
    Mistral,
    OpenRouter,
    /// Any server speaking the OpenAI-compatible API: LiteLLM, Ollama, vLLM, LM Studio, …
    OpenAiCompatible,
}

/// What the UI needs to know about a kind of provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiKindInfo {
    pub kind: AiKind,
    pub label: String,
    /// The vendor's address; none for a custom endpoint, which must be given one.
    pub default_base_url: Option<String>,
    pub needs_key: bool,
    /// Whether it makes embeddings (Anthropic doesn't).
    pub embeds: bool,
}

/// Something AI does, each with its own provider and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AiFeature {
    Chat,
    Embeddings,
}

impl AiFeature {
    pub const ALL: [Self; 2] = [Self::Chat, Self::Embeddings];

    pub fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embeddings => "embeddings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiChoice {
    pub provider: String,
    pub model: String,
}

/// A provider set up on this device. Its key is never sent back, only whether there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    pub id: String,
    pub kind: AiKind,
    pub name: String,
    pub base_url: Option<String>,
    /// The key's last four characters, when there is one.
    pub key_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiFeatureChoice {
    pub feature: AiFeature,
    pub choice: Option<AiChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiModel {
    pub id: String,
    pub name: Option<String>,
    /// Whether it looks like an embedding model.
    pub embeddings: bool,
}

/// One step of *Test connection*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AiCheckStep {
    pub what: String,
    pub ok: bool,
    pub detail: String,
    pub millis: u32,
}

/// What an agent may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccess {
    /// It may add to and edit pages, not only read them.
    pub write: bool,
    /// It may edit journal entries too.
    pub journal: bool,
}

/// Where an agent call happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AgentSource {
    /// Through this computer's local MCP endpoint.
    Device,
    /// Through the server's.
    Server,
}

/// An agent connected to this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentToken {
    pub id: String,
    pub name: String,
    pub access: AgentAccess,
    /// Unix milliseconds.
    #[specta(type = u32)]
    pub created_at: i64,
    #[specta(type = Option<u32>)]
    pub last_used_at: Option<i64>,
}

/// One agent call that changed something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentCall {
    pub id: String,
    pub source: AgentSource,
    /// The agent's name.
    pub agent: String,
    pub tool: String,
    /// What it did, in a sentence: "Added 2 blocks to “Groceries”".
    pub summary: String,
    /// Unix milliseconds.
    #[specta(type = u32)]
    pub at: i64,
    #[specta(type = Option<u32>)]
    pub undone_at: Option<i64>,
    /// The pages it touched.
    pub pages: Vec<PageId>,
}

/// The local MCP endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LocalAgents {
    /// This build can run one (desktops).
    pub available: bool,
    pub enabled: bool,
    pub port: u16,
    /// Where agents connect: `http://127.0.0.1:<port>/mcp`.
    pub url: String,
    /// Why it isn't running, when it should be.
    pub problem: Option<String>,
}

/// The port the local MCP endpoint uses unless the person picks another.
pub const LOCAL_AGENTS_PORT: u16 = 47_321;

/// A page that matched a search, at the block that matched best (none when only the title did).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub page: PageId,
    pub title: String,
    pub block: Option<BlockId>,
    pub snippet: Vec<SnippetPart>,
}

/// The Markdown bridge on this device.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BridgeStatus {
    /// The folder pages are written to; none while the bridge is off.
    pub folder: Option<String>,
    /// How the last round went, if one has run since the app started.
    pub last: Option<BridgeRound>,
}

/// What one round of the Markdown bridge did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRound {
    /// Files whose edits came back into their pages.
    pub merged: u32,
    /// Pages made for new files.
    pub created: u32,
    /// Pages moved to the trash because their files were deleted.
    pub trashed: u32,
    /// Files written.
    pub written: u32,
    /// Files it couldn't read or write.
    pub problems: Vec<String>,
    /// Why the round failed, if it did.
    pub error: Option<String>,
}

/// A page in the trash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashItem {
    pub id: PageId,
    pub title: String,
    /// The page it goes back under when restored.
    pub parent: Option<PageId>,
    /// When it went in the trash, in Unix milliseconds. (A plain number for TypeScript: these
    /// are never NaN, which is what would make specta's `f64` nullable.)
    #[specta(type = u32)]
    pub trashed_at: f64,
    /// When it will be deleted for good, in Unix milliseconds.
    #[specta(type = u32)]
    pub deleted_at: f64,
    /// How many pages went with it.
    pub subpages: u32,
}

/// The settings that live in the workspace, so they sync with the account and a new device has
/// them after signing in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// The page new journal entries go under. Made on first use.
    pub journal_parent: Option<PageId>,
    /// The page new journal entries are copied from: its blocks, with `{{date}}`,
    /// `{{title}}` and `{{weekday}}` filled in.
    pub journal_template: Option<PageId>,
    /// The first day of the week in calendars: 0 is Sunday, 1 Monday.
    pub week_start: u8,
    /// Whether Ask your notes may read journal entries. Off unless switched on.
    pub ask_journal: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            journal_parent: None,
            journal_template: None,
            week_start: 1,
            ask_journal: false,
        }
    }
}

/// One setting's new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Setting {
    JournalParent(Option<PageId>),
    JournalTemplate(Option<PageId>),
    WeekStart(u8),
    AskJournal(bool),
}

/// One change to a page, as the editor makes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Edit {
    InsertText {
        block: BlockId,
        pos: u32,
        text: String,
    },
    DeleteText {
        block: BlockId,
        pos: u32,
        len: u32,
    },
    Format {
        block: BlockId,
        start: u32,
        end: u32,
        mark: Mark,
        on: bool,
    },
    /// Enter: the text after `pos` moves to a new block of kind `kind`.
    Split {
        block: BlockId,
        pos: u32,
        kind: BlockKind,
    },
    /// A new empty block after `after`, or at the top.
    InsertBlock {
        after: Option<BlockId>,
        kind: BlockKind,
    },
    /// Backspace at the start of a block: join it onto the block before it.
    Join {
        block: BlockId,
    },
    DeleteBlock {
        block: BlockId,
    },
    SetKind {
        block: BlockId,
        kind: BlockKind,
    },
    SetIndent {
        block: BlockId,
        indent: u8,
    },
    /// Moves a block to just after `after`, or to the top.
    MoveBlock {
        block: BlockId,
        after: Option<BlockId>,
    },
}

/// The core's answer to a [`Command`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Response {
    Pages {
        pages: Vec<PageInfo>,
    },
    PageCreated {
        page: PageInfo,
    },
    Opened {
        view: PageView,
    },
    Edited {
        outcome: EditOutcome,
    },
    Account {
        status: AccountStatus,
    },
    /// The journal entry for a day; `created` when it was made just now.
    JournalDay {
        page: PageInfo,
        created: bool,
    },
    Settings {
        settings: Settings,
    },
    SearchResults {
        hits: Vec<SearchHit>,
    },
    Trash {
        pages: Vec<TrashItem>,
    },
    Attachment {
        kind: BlockKind,
    },
    /// A file, in base64.
    AttachmentData {
        data: String,
    },
    Bridge {
        status: BridgeStatus,
    },
    Imported {
        report: crate::import::ImportReport,
    },
    Agents {
        local: LocalAgents,
        tokens: Vec<AgentToken>,
        /// Newest first.
        activity: Vec<AgentCall>,
        /// Why the server's activity couldn't be listed, if it couldn't.
        server_problem: Option<String>,
    },
    AgentToken {
        token: String,
        url: String,
    },
    Ai {
        /// Whether this build can reach AI providers.
        available: bool,
        kinds: Vec<AiKindInfo>,
        providers: Vec<AiProvider>,
        features: Vec<AiFeatureChoice>,
    },
    AiProviderSaved {
        provider: AiProvider,
    },
    AiModels {
        models: Vec<AiModel>,
    },
    AskStatus {
        status: AskStatus,
    },
    Conversations {
        conversations: Vec<ConversationSummary>,
    },
    Conversation {
        conversation: Conversation,
    },
    AiCheck {
        steps: Vec<AiCheckStep>,
    },
    Share {
        share: ShareInfo,
    },
    DatabaseCreated {
        page: PageInfo,
        schema: Schema,
    },
    Database {
        schema: Schema,
    },
    View {
        result: crate::database::ViewResult,
    },
    RowCells {
        schema: Option<Schema>,
        row: Option<crate::database::RowView>,
    },
    FormulaCheck {
        check: crate::database::FormulaCheck,
    },
    NotionImported {
        report: crate::database::notion::NotionReport,
    },
    NotionToken {
        saved: bool,
    },
    Done,
}

/// Something that happened in the core that the UI didn't ask for, or that other views of the
/// same data need to hear about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The page tree or a title changed.
    PagesChanged { pages: Vec<PageInfo> },
    /// What's in the trash changed.
    TrashChanged { pages: Vec<TrashItem> },
    /// An agent on this computer changed something, or the agents' list changed.
    AgentsChanged,
    /// The AI providers or the features' choices changed.
    AiChanged,
    /// A file this device was waiting for has come from the server.
    AttachmentArrived { blob: String },
    /// The Markdown bridge ran a round that did something, or failed.
    BridgeChanged { status: BridgeStatus },
    /// An open page changed other than through the edits just answered (another device, or the
    /// part of a batch that was applied before one edit failed).
    PageChanged { page: PageId, patch: PagePatch },
    /// An open page had to be reloaded from its last saved state (after an internal error).
    /// The editor should replace what it shows with `view`.
    PageReloaded { view: PageView, reason: String },
    /// The account or sync status changed.
    AccountChanged { status: AccountStatus },
    /// The settings changed, here or on another device.
    SettingsChanged { settings: Settings },
    /// More of an answer to [`Command::Ask`].
    Ask { request: String, update: AskUpdate },
    /// How an import from Notion's API is going.
    NotionProgress { done: u32, total: u32, what: String },
}

/// A page's place in the tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub id: PageId,
    pub parent: Option<PageId>,
    pub title: String,
    pub children: Vec<PageId>,
    pub kind: PageKind,
    /// In name order.
    pub properties: Vec<Property>,
    /// Shared to the web, as the workspace last heard from the server.
    pub published: bool,
}

/// An open page: everything the editor needs to show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PageView {
    pub id: PageId,
    pub title: String,
    /// In display order.
    pub blocks: Vec<Block>,
}

/// What a batch of edits did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    /// The blocks the edits created (splits and inserted blocks), in the order they were made.
    pub created: Vec<BlockId>,
    pub patch: PagePatch,
}

/// The blocks that changed, and only those. Apply `removed`, then `upserted` in order: each
/// block goes right after its `after` block (or first, for `None`). When `order` is set, the
/// change can't be described block by block: show the blocks in that order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PagePatch {
    pub upserted: Vec<PlacedBlock>,
    pub removed: Vec<BlockId>,
    pub order: Option<Vec<BlockId>>,
}

impl PagePatch {
    pub fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty() && self.order.is_none()
    }
}

/// A block and the block shown before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlacedBlock {
    pub block: Block,
    pub after: Option<BlockId>,
}

/// How this device stands with its server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    /// The server this device is signed in to (or signing in to), as its public URL.
    pub server: Option<String>,
    /// The account's email address, once known.
    pub email: Option<String>,
    pub phase: SyncPhase,
    /// What went wrong, in words for the person, when something did.
    pub message: Option<String>,
}

impl AccountStatus {
    pub fn signed_out(message: Option<String>) -> Self {
        Self {
            server: None,
            email: None,
            phase: SyncPhase::SignedOut,
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    /// Not signed in: the notes live on this device only.
    SignedOut,
    /// Waiting for the person to finish signing in, in the browser.
    SigningIn,
    /// Connecting to the server.
    Connecting,
    /// Connected; changes go both ways as they happen.
    Online,
    /// Can't reach the server; trying again. Changes wait on the device.
    Offline,
    /// Stopped by something a retry won't fix (see the message).
    Failed,
}

/// Why a command failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnknownPage,
    UnknownBlock,
    /// A position past the end of a block, or inside a character.
    InvalidPosition,
    /// The command needs the page open first.
    PageNotOpen,
    /// A page can't move under itself.
    Cycle,
    /// The page was reloaded from its last saved state; the edit was not applied.
    Reloaded,
    /// Reading or writing the local store failed.
    Storage,
    /// This build can't do that (signing in, in the browser build).
    Unsupported,
    /// Signing in or out failed.
    Account,
    /// A value the command can't use (an empty property name, a template that isn't a page).
    Invalid,
    /// The file isn't on this device yet; it's being fetched.
    NotHere,
    Internal,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<drusite_crdt::Error> for ApiError {
    fn from(err: drusite_crdt::Error) -> Self {
        use drusite_crdt::Error as E;
        let code = match &err {
            E::UnknownBlock(_) => ErrorCode::UnknownBlock,
            E::UnknownPage(_) => ErrorCode::UnknownPage,
            E::OutOfRange { .. } | E::InsideCharacter(_) => ErrorCode::InvalidPosition,
            E::Cycle => ErrorCode::Cycle,
            E::Poisoned => ErrorCode::Reloaded,
            E::Invalid(_) => ErrorCode::Invalid,
            _ => ErrorCode::Internal,
        };
        Self::new(code, err.to_string())
    }
}

impl From<crate::Error> for ApiError {
    fn from(err: crate::Error) -> Self {
        match err {
            crate::Error::Document(e) => e.into(),
            crate::Error::UnknownDoc(_) => Self::new(ErrorCode::UnknownPage, err.to_string()),
            other => Self::new(ErrorCode::Storage, other.to_string()),
        }
    }
}

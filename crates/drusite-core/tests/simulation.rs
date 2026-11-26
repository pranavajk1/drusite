// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic simulation of sync under network chaos, with turmoil.
//!
//! Real cores, each with its own encrypted store on disk, run the real sync logic against an
//! in-memory model of the server. The model keeps the server's rules: a log per document with
//! sequence numbers, creation by the first snapshot, delivery after a cursor that skips the
//! connection's own pushes, catch-up from a compacted snapshot. Its log outlives crashes, as
//! committed rows in Postgres do. (The real server and Postgres are tested end to end in
//! `drusite-server/tests/sync.rs`; a simulated network can't host them.)
//!
//! The chaos, all driven by the seed:
//! - connections broken at random, and partitions between a device and the server;
//! - latency that varies per message, so devices' messages interleave in every order;
//! - duplicated frames, both ways;
//! - devices crashed and restarted from their stores, and the server crashed and restarted;
//! - pushes the server fails now and then (a database hiccup), so later pushes built on them
//!   are refused for missing changes and devices must recover.
//!
//! Every edit is a unique token. When the chaos stops and the network heals, every device must
//! hold every token that was ever saved, exactly once, and all devices must agree.
//!
//! `DRUSITE_SIM_SEEDS=n` runs seeds 1 to n (default 3, about 45 s each);
//! `DRUSITE_SIM_SEED=s` runs just seed s. Run many before changing sync.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use drusite_core::api::{Command, Edit, Response};
use drusite_core::{Core, StoreKey};
use drusite_model::{BlockKind, PageId, WorkspaceId};
use drusite_proto::{
    Ack, ClientMessage, DocKind, Entry, ErrorCode, ServerMessage, Subscribed, Updates,
    client_message, server_message::Body,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, broadcast};
use turmoil::net::{TcpListener, TcpStream};
use uuid::Uuid;

const PORT: u16 = 9000;
const DEVICES: usize = 3;
/// The model compacts sooner than the real server, so catch-ups from snapshots happen often.
const COMPACT_AFTER: u64 = 12;

/// Compactions put off because an update was still waiting for another writer's changes.
static DEFERRED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Pushes the model refused for building on changes it lacked; devices recover from these.
static REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---------------------------------------------------------------------------------------------
// Frames over TCP: a length, a counter, then a `drusite-proto` frame. (The real transport is
// WebSocket; the frames inside are the same.)
//
// turmoil's TCP drops messages silently while a link is down and leaves the connection open.
// Real TCP never does that: it retransmits, or the connection breaks. The counter makes this
// transport behave like TCP: a missing frame ends the connection, and both ends start over.

struct Writer<W> {
    inner: W,
    next: u64,
}

impl<W: tokio::io::AsyncWrite + Unpin> Writer<W> {
    fn new(inner: W) -> Self {
        Self { inner, next: 0 }
    }

    async fn send(&mut self, message: &impl prost::Message) -> std::io::Result<()> {
        let frame = drusite_proto::encode(message);
        let mut bytes = Vec::with_capacity(frame.len() + 12);
        bytes.extend(u32::try_from(frame.len()).unwrap().to_be_bytes());
        bytes.extend(self.next.to_be_bytes());
        bytes.extend(frame);
        self.next += 1;
        self.inner.write_all(&bytes).await
    }
}

struct Reader<R> {
    inner: R,
    expected: u64,
}

impl<R: tokio::io::AsyncRead + Unpin> Reader<R> {
    fn new(inner: R) -> Self {
        Self { inner, expected: 0 }
    }

    async fn recv<M: prost::Message + Default>(&mut self) -> std::io::Result<M> {
        let mut header = [0u8; 12];
        self.inner.read_exact(&mut header).await?;
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let counter = u64::from_be_bytes(header[4..].try_into().unwrap());
        if counter != self.expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                format!("lost frames {}..{counter}", self.expected),
            ));
        }
        self.expected += 1;
        let mut frame = vec![0u8; len];
        self.inner.read_exact(&mut frame).await?;
        drusite_proto::decode(&frame).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

// ---------------------------------------------------------------------------------------------
// The server model.

#[derive(Default)]
struct ModelDoc {
    /// Every version this document's log reaches.
    known: loro::VersionVector,
    workspace: Uuid,
    head: u64,
    snapshot: Option<(u64, Vec<u8>)>,
    updates: BTreeMap<u64, (Vec<u8>, Uuid)>,
}

/// What the server has committed: survives the server crashing.
#[derive(Default)]
struct ModelStore {
    docs: HashMap<Uuid, ModelDoc>,
    workspaces: HashSet<Uuid>,
}

impl ModelStore {
    /// Appends an update, or refuses one that would leave a hole in the log (and says what the
    /// log holds), as the real server does.
    fn append(&mut self, doc: Uuid, data: Vec<u8>, origin: Uuid) -> Result<u64, Vec<u8>> {
        let d = self.docs.get_mut(&doc).unwrap();
        let meta = loro::LoroDoc::decode_import_blob_meta(&data, true).unwrap();
        if !d.known.includes_vv(&meta.partial_start_vv) {
            REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(d.known.encode());
        }
        d.known.merge(&meta.partial_end_vv);
        d.head += 1;
        d.updates.insert(d.head, (data, origin));
        if d.head - d.snapshot.as_ref().map_or(0, |s| s.0) >= COMPACT_AFTER {
            let loro = loro::LoroDoc::new();
            if let Some((_, s)) = &d.snapshot {
                loro.import(s).unwrap();
            }
            let batch: Vec<Vec<u8>> = d.updates.values().map(|(u, _)| u.clone()).collect();
            let status = loro.import_batch(&batch).unwrap();
            // As the real server: an update can depend on another writer's changes that haven't
            // arrived yet (pushed again after a failure). Wait for them rather than compact them
            // away; the log is kept whole.
            if status.pending.is_none() {
                d.snapshot = Some((d.head, loro.export(loro::ExportMode::Snapshot).unwrap()));
                d.updates.clear();
            } else {
                DEFERRED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(d.head)
    }

    /// Entries after `after`, leaving out (but passing) those from `skip`; and the new cursor.
    fn entries_after(&self, doc: Uuid, after: u64, skip: Option<Uuid>) -> (Vec<Entry>, u64) {
        let Some(d) = self.docs.get(&doc) else {
            return (Vec::new(), after);
        };
        let mut out = Vec::new();
        let mut high = after;
        let mut from = after;
        if let Some((through, data)) = &d.snapshot
            && *through > after
        {
            out.push(Entry {
                seq: *through,
                data: data.clone(),
                snapshot: true,
            });
            from = *through;
            high = *through;
        }
        for (&seq, (data, origin)) in d.updates.range(from + 1..) {
            high = seq;
            if Some(*origin) != skip {
                out.push(Entry {
                    seq,
                    data: data.clone(),
                    snapshot: false,
                });
            }
        }
        (out, high)
    }
}

fn server_host(
    store: Arc<Mutex<ModelStore>>,
    dup_rate: f64,
    seed: u64,
) -> impl Fn() -> std::pin::Pin<Box<dyn Future<Output = turmoil::Result>>> {
    move || {
        let store = Arc::clone(&store);
        Box::pin(async move {
            let listener = TcpListener::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), PORT)).await?;
            // Announcements live in memory: a crash loses them, like a node's listener.
            let (announce, _) = broadcast::channel::<Uuid>(1024);
            let mut n = 0u64;
            loop {
                let (stream, _) = listener.accept().await?;
                n += 1;
                let store = Arc::clone(&store);
                let announce = announce.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(
                        stream,
                        store,
                        announce,
                        fastrand::Rng::with_seed(seed ^ n),
                        dup_rate,
                    )
                    .await;
                });
            }
        })
    }
}

fn subscribe(
    store: &Mutex<ModelStore>,
    s: drusite_proto::Subscribe,
    origin: Uuid,
    subs: &mut HashMap<Uuid, u64>,
    announce: &broadcast::Sender<Uuid>,
) -> Vec<ServerMessage> {
    let doc = Uuid::from_slice(&s.doc).unwrap();
    let ws = Uuid::from_slice(&s.workspace).unwrap();
    let mut st = store.lock().unwrap();
    let mut created = false;
    if !st.docs.contains_key(&doc) {
        // A page not created yet: the subscription waits for it, as on the real server.
        if s.create.is_empty() && s.kind == DocKind::Page as i32 {
            subs.insert(doc, s.after_seq);
            return vec![ServerMessage::new(Body::Subscribed(Subscribed {
                doc: s.doc,
                head_seq: 0,
                created: false,
                writable: true,
                version: Vec::new(),
            }))];
        }
        if s.create.is_empty() {
            return vec![error(ErrorCode::NotFound, &s.doc)];
        }
        if s.kind == DocKind::Workspace as i32 {
            st.workspaces.insert(ws);
        }
        st.docs.insert(
            doc,
            ModelDoc {
                workspace: ws,
                ..ModelDoc::default()
            },
        );
        st.append(doc, s.create.clone(), origin).unwrap();
        // Like the real server's NOTIFY: subscriptions waiting for this page get it.
        let _ = announce.send(doc);
        created = true;
    } else if st.docs[&doc].workspace != ws {
        return vec![error(ErrorCode::BadRequest, &s.doc)];
    }
    let skip = created.then_some(origin);
    let (entries, high) = st.entries_after(doc, s.after_seq, skip);
    subs.insert(doc, high.max(s.after_seq));
    let mut out = Vec::new();
    if !entries.is_empty() {
        out.push(ServerMessage::new(Body::Updates(Updates {
            doc: s.doc.clone(),
            entries,
        })));
    }
    let version = st.docs[&doc].known.encode();
    out.push(ServerMessage::new(Body::Subscribed(Subscribed {
        doc: s.doc,
        head_seq: high,
        created,
        writable: true,
        version,
    })));
    out
}

fn deliver(
    store: &Mutex<ModelStore>,
    docs: Vec<Uuid>,
    origin: Uuid,
    subs: &mut HashMap<Uuid, u64>,
) -> Vec<ServerMessage> {
    let st = store.lock().unwrap();
    let mut out = Vec::new();
    for doc in docs {
        if let Some(cursor) = subs.get_mut(&doc) {
            let (entries, high) = st.entries_after(doc, *cursor, Some(origin));
            *cursor = (*cursor).max(high);
            if !entries.is_empty() {
                out.push(ServerMessage::new(Body::Updates(Updates {
                    doc: doc.as_bytes().to_vec(),
                    entries,
                })));
            }
        }
    }
    out
}

async fn serve_connection(
    stream: TcpStream,
    store: Arc<Mutex<ModelStore>>,
    announce: broadcast::Sender<Uuid>,
    mut rng: fastrand::Rng,
    dup_rate: f64,
) -> std::io::Result<()> {
    let origin = Uuid::now_v7();
    let (r, w) = stream.into_split();
    let (mut r, mut w) = (Reader::new(r), Writer::new(w));
    let mut subs: HashMap<Uuid, u64> = HashMap::new();
    let mut heard = announce.subscribe();
    loop {
        let out: Vec<ServerMessage> = tokio::select! {
            message = r.recv::<ClientMessage>() => {
                match message?.body {
                    Some(client_message::Body::Subscribe(s)) => subscribe(&store, s, origin, &mut subs, &announce),
                    Some(client_message::Body::Push(p)) => {
                        let doc = Uuid::from_slice(&p.doc).unwrap();
                        if subs.contains_key(&doc) && rng.f64() < 0.03 {
                            // A database hiccup: nothing stored, the push refused.
                            vec![ServerMessage::new(Body::Error(drusite_proto::Error {
                                code: ErrorCode::Internal as i32,
                                message: "hiccup".to_owned(),
                                doc: p.doc,
                                push_id: p.push_id,
                                version: Vec::new(),
                            }))]
                        } else if subs.contains_key(&doc) {
                            let appended = store.lock().unwrap().append(doc, p.update, origin);
                            match appended {
                                Ok(seq) => {
                                    let _ = announce.send(doc);
                                    vec![ServerMessage::new(Body::Ack(Ack { doc: p.doc, push_id: p.push_id, seq }))]
                                }
                                Err(version) => vec![ServerMessage::new(Body::Error(drusite_proto::Error {
                                    code: ErrorCode::MissingDependencies as i32,
                                    message: "gap".to_owned(),
                                    doc: p.doc,
                                    push_id: p.push_id,
                                    version,
                                }))],
                            }
                        } else {
                            vec![error(ErrorCode::NotSubscribed, &p.doc)]
                        }
                    }
                    Some(client_message::Body::Unsubscribe(u)) => {
                        subs.remove(&Uuid::from_slice(&u.doc).unwrap());
                        Vec::new()
                    }
                    Some(client_message::Body::Ping(p)) => {
                        vec![ServerMessage::new(Body::Pong(drusite_proto::Pong { nonce: p.nonce }))]
                    }
                    None => Vec::new(),
                }
            }
            doc = heard.recv() => {
                let docs = match doc {
                    Ok(d) => vec![d],
                    Err(_) => subs.keys().copied().collect(),
                };
                deliver(&store, docs, origin, &mut subs)
            }
        };
        for m in out {
            w.send(&m).await?;
            if rng.f64() < dup_rate {
                w.send(&m).await?;
            }
        }
    }
}

fn error(code: ErrorCode, doc: &[u8]) -> ServerMessage {
    ServerMessage::new(Body::Error(drusite_proto::Error {
        code: code as i32,
        message: format!("{code:?}"),
        doc: doc.to_vec(),
        push_id: 0,
        version: Vec::new(),
    }))
}

// ---------------------------------------------------------------------------------------------
// Devices.

/// What the test sees of a device from outside the simulation.
#[derive(Default)]
struct DeviceSlot {
    /// The running incarnation's core; replaced when the device restarts.
    core: Option<Arc<Mutex<Core>>>,
    /// Every token this device saved. Saved means on disk: it must survive anything.
    saved: Vec<String>,
    incarnation: u64,
    /// Why sessions ended, most recent last.
    ended: Vec<String>,
    /// The current session: when it started, frames in and out.
    session: (Duration, u64, u64),
}

struct Shared {
    /// The account's workspace, once device 0 has made it.
    workspace: Mutex<Option<WorkspaceId>>,
    /// While true, devices keep editing.
    editing: Mutex<bool>,
    key: StoreKey,
}

fn open_core(dir: &std::path::Path, key: &StoreKey) -> Core {
    Core::open(dir, key, Box::new(|_| {})).unwrap()
}

fn page_ids(core: &mut Core) -> Vec<PageId> {
    let Response::Pages { pages } = core.handle(Command::ListPages).unwrap() else {
        panic!()
    };
    pages.into_iter().map(|p| p.id).collect()
}

/// Every block's text on every page, by page.
fn contents(core: &mut Core) -> BTreeMap<PageId, Vec<String>> {
    let mut out = BTreeMap::new();
    for page in page_ids(core) {
        let Ok(Response::Opened { view }) = core.handle(Command::OpenPage { page }) else {
            continue;
        };
        core.handle(Command::ClosePage { page }).unwrap();
        out.insert(
            page,
            view.blocks.iter().map(drusite_model::Block::text).collect(),
        );
    }
    out
}

/// One random edit; returns the token it saved, if it saved one. Inserts only at token
/// boundaries, so no token is ever split by another.
fn random_edit(core: &mut Core, rng: &mut fastrand::Rng, token: &str) -> Option<String> {
    let pages = page_ids(core);
    if pages.is_empty() || rng.u8(..10) == 0 {
        let title = token.to_owned();
        return matches!(
            core.handle(Command::CreatePage {
                parent: None,
                title
            }),
            Ok(Response::PageCreated { .. })
        )
        .then(|| token.to_owned());
    }
    let page = pages[rng.usize(..pages.len())];
    let Ok(Response::Opened { view }) = core.handle(Command::OpenPage { page }) else {
        return None;
    };
    let block = &view.blocks[rng.usize(..view.blocks.len())];
    let text = block.text();
    // Boundaries: the start, and after each token's closing bracket.
    let mut boundaries = vec![0u32];
    let mut pos = 0u32;
    for c in text.chars() {
        pos += u32::try_from(c.len_utf16()).unwrap();
        if c == '⟧' {
            boundaries.push(pos);
        }
    }
    let at = boundaries[rng.usize(..boundaries.len())];
    let edit = if rng.u8(..8) == 0 {
        Edit::Split {
            block: block.id,
            pos: at,
            kind: BlockKind::Paragraph,
        }
    } else {
        Edit::InsertText {
            block: block.id,
            pos: at,
            text: token.to_owned(),
        }
    };
    let insert = matches!(&edit, Edit::InsertText { .. });
    let saved = core
        .handle(Command::Edit {
            page,
            edits: vec![edit],
        })
        .is_ok()
        && insert;
    let _ = core.handle(Command::ClosePage { page });
    saved.then(|| token.to_owned())
}

fn device_host(
    index: usize,
    dir: PathBuf,
    shared: Arc<Shared>,
    slot: Arc<Mutex<DeviceSlot>>,
    seed: u64,
    dup_rate: f64,
) -> impl Fn() -> std::pin::Pin<Box<dyn Future<Output = turmoil::Result>>> {
    move || {
        let dir = dir.clone();
        let shared = Arc::clone(&shared);
        let slot = Arc::clone(&slot);
        Box::pin(async move {
            // A (re)start: open the store from disk, as the app does.
            let core = Arc::new(Mutex::new(open_core(&dir, &shared.key)));
            let incarnation = {
                let mut s = slot.lock().unwrap();
                s.incarnation += 1;
                s.core = Some(Arc::clone(&core));
                s.incarnation
            };
            let mut rng = fastrand::Rng::with_seed(seed ^ ((index as u64) << 32) ^ incarnation);
            let changed = Arc::new(Notify::new());
            {
                let notify = Arc::clone(&changed);
                core.lock()
                    .unwrap()
                    .set_change_listener(Some(Box::new(move || notify.notify_one())));
            }
            if index == 0 {
                let mut ws = shared.workspace.lock().unwrap();
                if ws.is_none() {
                    *ws = Some(core.lock().unwrap().workspace_id());
                }
            }

            // Editing, online or not.
            let editor = {
                let core = Arc::clone(&core);
                let shared = Arc::clone(&shared);
                let slot = Arc::clone(&slot);
                let mut rng = fastrand::Rng::with_seed(rng.u64(..));
                tokio::spawn(async move {
                    let mut n = 0;
                    loop {
                        tokio::time::sleep(Duration::from_millis(rng.u64(20..400))).await;
                        if !*shared.editing.lock().unwrap() {
                            continue;
                        }
                        let mut core = core.lock().unwrap();
                        // A device edits only once it holds the account's workspace.
                        if Some(core.workspace_id()) != *shared.workspace.lock().unwrap() {
                            continue;
                        }
                        n += 1;
                        let token = format!("⟦{index}.{incarnation}.{n}⟧");
                        if let Some(t) = random_edit(&mut core, &mut rng, &token) {
                            slot.lock().unwrap().saved.push(t);
                        }
                    }
                })
            };

            // Connecting, and reconnecting whenever the connection breaks.
            loop {
                let ws = *shared.workspace.lock().unwrap();
                let join = {
                    let core = core.lock().unwrap();
                    match ws {
                        Some(w) if core.workspace_id() != w => Some(w),
                        _ => None,
                    }
                };
                if index != 0 && ws.is_none() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                slot.lock().unwrap().session = (turmoil::elapsed(), 0, 0);
                let result = session(&core, join, &changed, &mut rng, dup_rate, &slot).await;
                if let Err(e) = result {
                    slot.lock()
                        .unwrap()
                        .ended
                        .push(format!("{:?} {e}", turmoil::elapsed()));
                }
                core.lock().unwrap().sync_stop();
                tokio::time::sleep(Duration::from_millis(rng.u64(50..500))).await;
                if editor.is_finished() {
                    break;
                }
            }
            Ok(())
        })
    }
}

async fn session(
    core: &Arc<Mutex<Core>>,
    join: Option<WorkspaceId>,
    changed: &Notify,
    rng: &mut fastrand::Rng,
    dup_rate: f64,
    slot: &Arc<Mutex<DeviceSlot>>,
) -> Result<(), String> {
    let stream = TcpStream::connect(("server", PORT))
        .await
        .map_err(|e| format!("connect: {e:?}"))?;
    let (r, w) = stream.into_split();
    let (mut r, mut w) = (Reader::new(r), Writer::new(w));
    let first = {
        let mut core = core.lock().unwrap();
        match join {
            Some(ws) => core.sync_start_join(ws),
            None => core.sync_start(),
        }
        .map_err(|e| e.to_string())?
    };
    // As the real driver: reading never waits on writing; a writer task drains a queue.
    let (outgoing, mut queue) = tokio::sync::mpsc::unbounded_channel::<ClientMessage>();
    let mut writer = {
        let slot = Arc::clone(slot);
        let mut rng = fastrand::Rng::with_seed(rng.u64(..));
        tokio::spawn(async move {
            while let Some(m) = queue.recv().await {
                slot.lock().unwrap().session.2 += 1;
                let times = if rng.f64() < dup_rate { 2 } else { 1 };
                for _ in 0..times {
                    tokio::time::timeout(Duration::from_secs(5), w.send(&m))
                        .await
                        .map_err(|_| "send timed out".to_owned())?
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok::<(), String>(())
        })
    };
    let queue_all = |messages: Vec<ClientMessage>| {
        for m in messages {
            let _ = outgoing.send(m);
        }
    };
    queue_all(first);
    // As the real driver: ping, and give up on a connection that has gone silent.
    let mut heard = tokio::time::Instant::now();
    let mut nonce = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    let result = loop {
        tokio::select! {
            message = r.recv::<ServerMessage>() => {
                heard = tokio::time::Instant::now();
                slot.lock().unwrap().session.1 += 1;
                let message = match message {
                    Ok(m) => m,
                    Err(e) => break Err(e.to_string()),
                };
                match core.lock().unwrap().sync_receive(message) {
                    Ok(replies) => queue_all(replies),
                    Err(e) => break Err(e.to_string()),
                }
            }
            () = changed.notified() => {
                match core.lock().unwrap().sync_outgoing() {
                    Ok(out) => queue_all(out),
                    Err(e) => break Err(e.to_string()),
                }
            }
            _ = tick.tick() => {
                if heard.elapsed() > Duration::from_secs(5) {
                    break Err("silent".to_owned());
                }
                nonce += 1;
                queue_all(vec![ClientMessage::new(client_message::Body::Ping(drusite_proto::Ping { nonce }))]);
                match core.lock().unwrap().sync_retry() {
                    Ok(out) => queue_all(out),
                    Err(e) => break Err(e.to_string()),
                }
            }
            ended = &mut writer => {
                break match ended {
                    Ok(r) => r,
                    Err(e) => Err(e.to_string()),
                };
            }
        }
    };
    writer.abort();
    result
}

// ---------------------------------------------------------------------------------------------
// The scenario.

fn run_seed(seed: u64) {
    let mut rng = fastrand::Rng::with_seed(seed);
    let dup_rate = 0.05;
    let mut sim = turmoil::Builder::new()
        .rng_seed(seed)
        .simulation_duration(Duration::from_secs(1800))
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(150))
        .fail_rate(0.002)
        .repair_rate(0.5)
        .enable_random_order()
        .build();

    let store = Arc::new(Mutex::new(ModelStore::default()));
    sim.host("server", server_host(Arc::clone(&store), dup_rate, seed));

    let dirs: Vec<tempfile::TempDir> = (0..DEVICES).map(|_| tempfile::tempdir().unwrap()).collect();
    let shared = Arc::new(Shared {
        workspace: Mutex::new(None),
        editing: Mutex::new(true),
        key: StoreKey::generate().unwrap(),
    });
    let slots: Vec<Arc<Mutex<DeviceSlot>>> = (0..DEVICES).map(|_| Arc::default()).collect();
    let names: Vec<String> = (0..DEVICES).map(|i| format!("device{i}")).collect();
    for i in 0..DEVICES {
        sim.host(
            names[i].as_str(),
            device_host(
                i,
                dirs[i].path().to_owned(),
                Arc::clone(&shared),
                Arc::clone(&slots[i]),
                seed,
                dup_rate,
            ),
        );
    }

    // Chaos for two simulated minutes.
    let chaos_until = Duration::from_secs(120);
    let mut partitioned: HashSet<usize> = HashSet::new();
    let mut crashed: Vec<(usize, Duration)> = Vec::new();
    let mut server_down_until: Option<Duration> = None;
    let mut next = Duration::from_secs(2);
    let verbose = std::env::var_os("DRUSITE_SIM_VERBOSE").is_some();
    let mut shown = Duration::ZERO;
    let started = std::time::Instant::now();
    while sim.elapsed() < chaos_until {
        sim.step().unwrap();
        let now = sim.elapsed();
        if verbose && now >= shown + Duration::from_secs(10) {
            shown = now;
            eprintln!("  sim {now:?} after {:?} real", started.elapsed());
        }
        // Bring back what's due.
        crashed.retain(|&(i, until)| {
            if now >= until {
                sim.bounce(names[i].as_str());
                false
            } else {
                true
            }
        });
        if server_down_until.is_some_and(|t| now >= t) {
            sim.bounce("server");
            server_down_until = None;
        }
        if now < next {
            continue;
        }
        next = now + Duration::from_millis(rng.u64(500..4000));
        let i = rng.usize(..DEVICES);
        match rng.u8(..10) {
            0..=3 => {
                if partitioned.remove(&i) {
                    sim.repair(names[i].as_str(), "server");
                } else {
                    sim.partition(names[i].as_str(), "server");
                    partitioned.insert(i);
                }
            }
            4 | 5 if !crashed.iter().any(|&(c, _)| c == i) => {
                sim.crash(names[i].as_str());
                crashed.push((i, now + Duration::from_millis(rng.u64(200..3000))));
            }
            6 if server_down_until.is_none() => {
                sim.crash("server");
                server_down_until = Some(now + Duration::from_millis(rng.u64(200..3000)));
            }
            _ => {}
        }
    }

    // Heal everything and stop editing; then everyone must agree and have everything.
    *shared.editing.lock().unwrap() = false;
    for i in partitioned.drain() {
        sim.repair(names[i].as_str(), "server");
    }
    for (i, _) in crashed.drain(..) {
        sim.bounce(names[i].as_str());
    }
    if server_down_until.take().is_some() {
        sim.bounce("server");
    }

    let mut saved: Vec<String> = Vec::new();
    for s in &slots {
        saved.extend(s.lock().unwrap().saved.iter().cloned());
    }
    assert!(
        saved.len() > 50,
        "seed {seed}: only {} edits were made",
        saved.len()
    );

    let settle_until = sim.elapsed() + Duration::from_secs(120);
    let mut last_problem = String::new();
    while sim.elapsed() < settle_until {
        for _ in 0..200 {
            sim.step().unwrap();
        }
        if verbose {
            eprintln!(
                "  settling {:?} after {:?} real",
                sim.elapsed(),
                started.elapsed()
            );
        }
        match check(&slots, &saved) {
            Ok(()) => {
                eprintln!(
                    "seed {seed}: {} edits by {DEVICES} devices survived and converged at {:?} \
                     ({} pushes refused for gaps and recovered, {} compactions put off)",
                    saved.len(),
                    sim.elapsed(),
                    REFUSED.swap(0, std::sync::atomic::Ordering::Relaxed),
                    DEFERRED.swap(0, std::sync::atomic::Ordering::Relaxed),
                );
                return;
            }
            Err(problem) => last_problem = problem,
        }
    }
    // Where did it go missing? Every device's count for each missing token, and the server's.
    let mut server_pages: HashSet<String> = HashSet::new();
    for (id, d) in &store.lock().unwrap().docs {
        if d.workspace == *id {
            let mut parts: Vec<&[u8]> = Vec::new();
            if let Some((_, snap)) = &d.snapshot {
                parts.push(snap);
            }
            parts.extend(d.updates.values().map(|(u, _)| u.as_slice()));
            let ws = drusite_crdt::Workspace::load_all(&parts).unwrap();
            server_pages = ws
                .pages()
                .unwrap()
                .into_iter()
                .map(|p| p.id.to_string())
                .collect();
            eprintln!(
                "server workspace doc {}: head {}, snapshot {:?}, {} pages",
                &id.to_string()[..8],
                d.head,
                d.snapshot.as_ref().map(|s| s.0),
                server_pages.len(),
            );
        }
    }
    let server_text: HashMap<Uuid, String> = store
        .lock()
        .unwrap()
        .docs
        .iter()
        .map(|(id, d)| {
            let loro = loro::LoroDoc::new();
            if let Some((_, snap)) = &d.snapshot {
                loro.import(snap).unwrap();
            }
            for (u, _) in d.updates.values() {
                let _ = loro.import(u);
            }
            (*id, format!("{:?}", loro.get_deep_value()))
        })
        .collect();
    let mut report = String::new();
    for (i, slot) in slots.iter().enumerate() {
        let s = slot.lock().unwrap();
        let core = s.core.clone().unwrap();
        let mut core = core.lock().unwrap();
        let ws = core.workspace_id();
        let pages = page_ids(&mut core).len();
        report.push_str(&format!(
            "\n  device {i}: workspace {}, {pages} pages, incarnation {}, saved {}, blank {}",
            &ws.to_string()[..8],
            s.incarnation,
            s.saved.len(),
            core.is_blank().unwrap()
        ));
        let mut kinds: HashMap<String, usize> = HashMap::new();
        for e in &s.ended {
            let kind = e.split_once(' ').map_or(e.as_str(), |(_, k)| k);
            *kinds.entry(kind.chars().take(90).collect()).or_default() += 1;
        }
        report.push_str(&format!(
            "\n    last ended: {:?}",
            s.ended.iter().rev().take(3).collect::<Vec<_>>()
        ));
        report.push_str(&format!(
            "\n    sessions ended: {kinds:?}; current session {:?}",
            s.session
        ));
        let st = core.sync_status().unwrap();
        let ws_status = &st[0];
        let not_ready = st.iter().filter(|d| !d.ready).count();
        let not_writable = st.iter().filter(|d| d.ready && !d.writable).count();
        report.push_str(&format!("\n    workspace doc {ws_status:?}; docs not ready {not_ready}, not writable {not_writable} of {}", st.len()));
        let stored: HashSet<String> = st.iter().map(|d| d.doc.clone()).collect();
        let Response::Pages { pages: tree } = core.handle(Command::ListPages).unwrap() else {
            panic!()
        };
        let missing: Vec<(String, String)> = tree
            .iter()
            .filter(|p| !stored.contains(&p.id.to_string()))
            .map(|p| (p.id.to_string()[..8].to_owned(), p.title.clone()))
            .collect();
        let on_server = missing
            .iter()
            .filter(|(id, _)| {
                store
                    .lock()
                    .unwrap()
                    .docs
                    .keys()
                    .any(|k| k.to_string().starts_with(id.as_str()))
            })
            .count();
        let tree_ids: HashSet<String> = tree.iter().map(|p| p.id.to_string()).collect();
        report.push_str(&format!(
            "\n    tree vs server tree: {} only here, {} only on the server",
            tree_ids.difference(&server_pages).count(),
            server_pages.difference(&tree_ids).count()
        ));
        report.push_str(&format!("\n    tree pages without a document here: {} ({on_server} of them on the server), e.g. {:?}", missing.len(), missing.iter().take(3).collect::<Vec<_>>()));
    }
    report.push_str(&format!(
        "\n  account workspace {:?}",
        shared
            .workspace
            .lock()
            .unwrap()
            .map(|w| w.to_string()[..8].to_owned())
    ));
    let device_text: Vec<String> = slots
        .iter()
        .map(|slot| {
            let core = slot.lock().unwrap().core.clone().unwrap();
            let mut core = core.lock().unwrap();
            let pages = contents(&mut core);
            let Response::Pages { pages: tree } = core.handle(Command::ListPages).unwrap() else {
                panic!()
            };
            let titles: Vec<String> = tree.into_iter().map(|p| p.title).collect();
            pages
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join("")
                + &titles.join("")
        })
        .collect();
    for token in &saved {
        let counts: Vec<usize> = device_text
            .iter()
            .map(|t| t.matches(token.as_str()).count())
            .collect();
        if counts.iter().any(|&c| c != 1) && report.matches("where:").count() < 3 {
            // Which page holds it on the devices that have it, and how each device stands on it.
            for (i, slot) in slots.iter().enumerate() {
                let core = slot.lock().unwrap().core.clone().unwrap();
                let mut core = core.lock().unwrap();
                let Response::Pages { pages: tree } = core.handle(Command::ListPages).unwrap()
                else {
                    panic!()
                };
                if let Some(p) = tree.iter().find(|p| p.title.contains(token.as_str())) {
                    report.push_str(&format!(
                        "\n    where: device {i} has it as the title of {}",
                        p.id
                    ));
                }
                for (page, blocks) in contents(&mut core) {
                    if blocks.iter().any(|b| b.contains(token.as_str())) {
                        let st = core.sync_status().unwrap();
                        let d = st.iter().find(|d| d.doc == page.to_string());
                        let server = store
                            .lock()
                            .unwrap()
                            .docs
                            .get(page.as_uuid())
                            .map(|d| (d.head, d.snapshot.as_ref().map(|s| s.0)));
                        report.push_str(&format!("\n    where: device {i} has it in page {page}; device status {d:?}; server (head, snapshot) {server:?}"));
                        for (j, other) in slots.iter().enumerate() {
                            if j == i {
                                continue;
                            }
                            let oc = other.lock().unwrap().core.clone().unwrap();
                            let oc = oc.lock().unwrap();
                            let os = oc.sync_status().unwrap();
                            report.push_str(&format!(
                                "\n      device {j} on that page: {:?}",
                                os.iter().find(|d| d.doc == page.to_string())
                            ));
                        }
                    }
                }
            }
        }
        if counts.iter().any(|&c| c != 1) {
            let on_server: Vec<String> = server_text
                .iter()
                .filter(|(_, t)| t.contains(token.as_str()))
                .map(|(id, _)| id.to_string()[..8].to_owned())
                .collect();
            report.push_str(&format!(
                "\n  {token}: devices {counts:?}, server docs {on_server:?}"
            ));
        }
    }
    panic!("seed {seed}: didn't converge: {last_problem}{report}");
}

/// Every device holds every saved token exactly once, and all agree.
fn check(slots: &[Arc<Mutex<DeviceSlot>>], saved: &[String]) -> Result<(), String> {
    let mut first: Option<BTreeMap<PageId, Vec<String>>> = None;
    for (i, slot) in slots.iter().enumerate() {
        let core = slot.lock().unwrap().core.clone().ok_or("no core")?;
        let mut core = core.lock().unwrap();
        let pages = contents(&mut core);
        let titles: Vec<String> = {
            let Response::Pages { pages } = core.handle(Command::ListPages).unwrap() else {
                panic!()
            };
            pages.into_iter().map(|p| p.title).collect()
        };
        let all: String = pages
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join("")
            + &titles.join("");
        for token in saved {
            let count = all.matches(token.as_str()).count();
            if count != 1 {
                return Err(format!("device {i} has {token} {count} times"));
            }
        }
        match &first {
            None => first = Some(pages),
            Some(f) if *f != pages => return Err(format!("device {i} disagrees with device 0")),
            Some(_) => {}
        }
    }
    Ok(())
}

#[test]
fn every_edit_survives_network_chaos() {
    let seeds: Vec<u64> = match std::env::var("DRUSITE_SIM_SEED") {
        Ok(s) => vec![s.parse().unwrap()],
        Err(_) => {
            let n: u64 = std::env::var("DRUSITE_SIM_SEEDS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3);
            (1..=n).collect()
        }
    };
    for seed in seeds {
        run_seed(seed);
    }
}

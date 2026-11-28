// SPDX-License-Identifier: AGPL-3.0-or-later

// The tab whose worker owns the store: the core, the session with the server, and the sync
// connection. Other tabs send it their commands (see worker.ts).
//
// The sync loop is `crates/drusite-core/src/sync/net.rs` and the account supervisor's, in the
// browser: the same pings, silence limit, backoff and "which workspace to join" rule.

import type { AccountStatus, ApiError, AskStatus, AskUpdate, Command, Event, Response, ShareInfo, SyncPhase } from "../types";
import init, { WasmCore, wipeStore } from "./wasm/drusite_wasm.js";
import { forgetKey, remember, remembered, storeKey } from "./keys";
import * as server from "./server";

const PING_EVERY = 30_000;
const SILENT_FOR = 75_000;
const BACKOFF_MIN = 1_000;
const BACKOFF_MAX = 5 * 60_000;
const FAILED_RETRY = 5 * 60_000;
const FILES_AT_ONCE = 4;
const SUBPROTOCOL = "drusite.sync.v1";
/** The close code the server ends a connection with when it signed this device out. */
const SIGNED_OUT_CLOSE = 4001;

type Result = { ok: Response } | { err: ApiError };

const accountError = (e: unknown): ApiError => ({ code: "account", message: e instanceof Error ? e.message : String(e) });

/** What the leader needs from the worker around it. */
export interface Host {
  /** Sends an event to every tab, or every tab but `except`. */
  emit(event: Event, except?: string): void;
  /** Sends the tab `to` (or every tab) to `url`. */
  navigate(url: string, to?: string): void;
}

export class Leader {
  private status: AccountStatus = { server: null, email: null, phase: "signedOut", message: null, serverVersion: null };
  private socket: WebSocket | null = null;
  private wake: (() => void) | null = null;
  private stopped = false;
  private moving = new Set<string>();
  private asking = new Map<string, AbortController>();
  /** Signed in, as far as the last word from the server goes. */
  private signedIn = false;

  private constructor(
    private core: WasmCore,
    private readonly host: Host,
    private readonly memory: WebAssembly.Memory,
  ) {}

  /** How big the core's WebAssembly memory is, in bytes. */
  get memoryBytes(): number {
    return this.memory.buffer.byteLength;
  }

  /** Opens the store (OPFS where the browser has it) and starts the session with the server. */
  static async start(host: Host): Promise<Leader> {
    const { memory } = await init();
    const { key, fresh } = await storeKey();
    // A tab that closed a moment ago may still hold OPFS's handles: try a few times.
    let core: WasmCore | null = null;
    let cleared = false;
    for (let attempt = 0; ; attempt++) {
      try {
        core = await WasmCore.open(key, true);
        break;
      } catch (e) {
        // A key made just now meeting a store from before: that store's key is gone (the
        // browser's site data was partly cleared), so nothing can read it. Start afresh; a
        // signed-in browser gets its notes back from the server.
        if (fresh && !cleared && String(e).includes("store key doesn't open")) {
          console.warn("drusite: this browser's store is from a key it no longer has; starting afresh");
          wipeStore();
          cleared = true;
          continue;
        }
        console.warn("drusite: opening the store", e);
        if (attempt >= 20) throw e;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
    const leader = new Leader(core, host, memory);
    void leader.run();
    return leader;
  }

  get persistent(): boolean {
    return this.core.persistent;
  }

  // ---- Commands ----

  /** Runs one command from tab `from`. */
  async handle(from: string, command: Command): Promise<Result> {
    try {
      const special = await this.account(from, command);
      if (special) return { ok: special };
    } catch (e) {
      return { err: isApiError(e) ? e : accountError(e) };
    }
    const result = this.run_core(command);
    // Another tab showing the page sees the edit as it would one from another device.
    if ("ok" in result && command.type === "edit" && result.ok.type === "edited" && !isEmpty(result.ok.outcome.patch))
      this.host.emit({ type: "pageChanged", page: command.page, patch: result.ok.outcome.patch }, from);
    return result;
  }

  private run_core(command: Command): Result {
    const result = JSON.parse(this.core.handle(JSON.stringify(command))) as Result;
    this.after();
    return result;
  }

  /** After every call into the core: pass its events on, and push what changed. */
  private after(): void {
    const events = JSON.parse(this.core.takeEvents()) as Event[];
    for (const e of events) this.host.emit(e);
    if (this.core.takeChanged()) this.pushChanges();
  }

  private get workspace(): string {
    return this.core.workspaceId();
  }

  /** The commands that need the server or the browser rather than the core, answered here. */
  private async account(from: string, command: Command): Promise<Response | null> {
    switch (command.type) {
      case "accountStatus":
        return { type: "account", status: this.status };
      case "signIn":
        this.host.navigate("/web/login", from);
        return { type: "done" };
      case "cancelSignIn":
        return { type: "done" };
      case "signOut":
        await this.signOut();
        return { type: "done" };
      case "agents": {
        const r = this.run_core(command);
        if ("err" in r) throw r.err;
        if (r.ok.type === "agents" && this.signedIn) {
          try {
            r.ok.activity.push(...(await server.agentActivity(this.workspace)));
            r.ok.activity.sort((a, b) => b.at - a.at);
          } catch (e) {
            r.ok.serverProblem = e instanceof Error ? e.message : String(e);
          }
        }
        return r.ok;
      }
      case "undoAgentCall":
        if (command.source !== "server") return null;
        await server.undoAgentCall(this.workspace, command.id);
        return { type: "done" };
      case "askStatus":
        return { type: "askStatus", status: await this.askStatus() };
      case "conversations":
        return { type: "conversations", conversations: await server.conversations(this.workspace) };
      case "conversation":
        return { type: "conversation", conversation: await server.conversation(this.workspace, command.id) };
      case "renameConversation":
        await server.renameConversation(this.workspace, command.id, command.title);
        return { type: "done" };
      case "deleteConversation":
        await server.deleteConversation(this.workspace, command.id);
        return { type: "done" };
      case "ask": {
        const control = new AbortController();
        this.asking.set(command.request, control);
        const update = (u: AskUpdate) => {
          if (control.signal.aborted) return;
          this.host.emit({ type: "ask", request: command.request, update: u });
          if (u.type === "done" || u.type === "failed") this.asking.delete(command.request);
        };
        const body = { conversation: command.conversation, question: command.question, today: command.today, page: command.page };
        await server.ask(this.workspace, body, control.signal, update);
        return { type: "done" };
      }
      case "cancelAsk":
        this.asking.get(command.request)?.abort();
        this.asking.delete(command.request);
        return { type: "done" };
      case "share":
        try {
          return { type: "share", share: await server.share(this.workspace, command.page) };
        } catch (e) {
          const local = JSON.parse(this.core.localShare(command.page)) as ShareInfo | { err: ApiError };
          if ("err" in local) throw local.err;
          local.message = this.signedIn ? server.shareWords(e) : "Sign in to a Drusite server to publish pages.";
          return { type: "share", share: local };
        }
      case "publish":
        return { type: "share", share: await shareCall(() => server.publish(this.workspace, command.page, command.change)) };
      case "unpublish":
        return { type: "share", share: await shareCall(() => server.unpublish(this.workspace, command.page)) };
      case "regenerateShareLink":
        return { type: "share", share: await shareCall(() => server.regenerateShareLink(this.workspace, command.page)) };
      default:
        return null;
    }
  }

  private async askStatus(): Promise<AskStatus> {
    const none = (reason: string, message: string, signedIn: boolean): AskStatus => ({
      signedIn,
      available: false,
      message,
      reason,
      model: null,
      semantic: false,
      embeddingModel: null,
      indexing: 0,
      pageScope: false,
    });
    if (!this.signedIn) return none("not_signed_in", "Sign in to a Drusite server to ask your notes.", false);
    try {
      const s = await server.askStatus(this.workspace);
      return {
        signedIn: true,
        available: s.available,
        message: s.message,
        reason: s.reason,
        model: s.model,
        semantic: s.semantic,
        embeddingModel: s.embeddingModel ?? null,
        indexing: Math.max(0, s.indexing),
        pageScope: s.pageScope ?? false,
      };
    } catch (e) {
      if (e instanceof server.Unreachable) return none("unreachable", "Not connected to the server right now.", true);
      if (e instanceof server.ServerError && e.code === "forbidden") return none("syncing", "Your notes are still syncing with the server.", true);
      return none("unreachable", e instanceof Error ? e.message : String(e), true);
    }
  }

  // ---- The account ----

  private setStatus(phase: SyncPhase, message: string | null = null): void {
    const next = { ...this.status, phase, message };
    if (JSON.stringify(next) === JSON.stringify(this.status)) return;
    this.status = next;
    this.host.emit({ type: "accountChanged", status: next });
  }

  /** Signs this browser out: on the server, then its copy of the notes goes. */
  private async signOut(): Promise<void> {
    try {
      await server.logout();
    } catch (e) {
      // Offline: the copy still goes. The server's session ends when it runs out, or from
      // Account → Devices on another device.
      if (!(e instanceof server.Unreachable)) throw e;
    }
    await this.wipe();
    this.host.navigate("/");
  }

  /** Closes the store and deletes it and its key. */
  private async wipe(): Promise<void> {
    this.stopped = true;
    this.socket?.close();
    this.socket = null;
    this.core.free();
    try {
      wipeStore();
    } finally {
      await forgetKey();
    }
  }

  /** Tells the loop to look again now (back online, a tab came forward). */
  kick(): void {
    this.wake?.();
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(done, ms);
      function done() {
        clearTimeout(timer);
        resolve();
      }
      this.wake = done;
    });
  }

  /** The supervisor's loop: one connection at a time, until the store is closed. */
  private async run(): Promise<void> {
    let backoff = BACKOFF_MIN;
    const grow = () => {
      const current = backoff;
      backoff = Math.min(backoff * 2, BACKOFF_MAX);
      return current + current * Math.random() * 0.25;
    };
    while (!this.stopped) {
      let session: Awaited<ReturnType<typeof server.session>>;
      try {
        session = await server.session();
      } catch {
        // Offline. A browser that was signed in still is, as far as it knows.
        const last = this.signedIn ? null : await remembered().catch(() => null);
        if (last) {
          this.signedIn = true;
          this.status = { ...this.status, server: last.server, email: last.email };
        }
        this.setStatus(this.signedIn ? "offline" : "signedOut", this.signedIn ? "Not connected to the server right now." : null);
        await this.sleep(grow());
        continue;
      }
      if (session === "noServer") {
        // The development server: notes stay in this browser.
        this.signedIn = false;
        this.setStatus("signedOut");
        return;
      }
      if (session === "signedOut") {
        // Signed out from another device: this browser's copy goes with the session.
        await this.wipe();
        this.host.navigate("/");
        return;
      }
      if (session === "none") {
        await remember(null).catch(() => {});
        // A first visit (or just signed out): straight to signing in. A browser with notes of
        // its own (its sign-in ran out) keeps them and offers Sign in under Settings.
        if (this.core.isBlank()) {
          this.host.navigate("/web/login");
          return;
        }
        this.signedIn = false;
        this.status = { ...this.status, server: location.origin + "/", email: null };
        this.setStatus("signedOut", null);
        await this.sleep(FAILED_RETRY);
        continue;
      }
      this.signedIn = true;
      this.status = { ...this.status, server: session.server, email: session.email };
      await remember({ server: session.server, email: session.email }).catch(() => {});
      const ended = await this.connect();
      if (this.stopped) return;
      if (ended === "online") backoff = BACKOFF_MIN;
      if (ended === "failed") await this.sleep(FAILED_RETRY);
      else await this.sleep(grow());
    }
  }

  /** One connection: how it ended. */
  private async connect(): Promise<"online" | "never" | "failed"> {
    this.setStatus("connecting");
    let join: string;
    try {
      join = this.core.workspaceToJoin(JSON.stringify(await server.workspaces()));
    } catch (e) {
      this.setStatus("offline", e instanceof Error ? e.message : String(e));
      return "never";
    }
    if (join === "conflict") {
      this.setStatus(
        "failed",
        "This device has notes of its own, and the account already has notes from another device. Drusite can't merge two sets of notes yet, so this device isn't syncing.",
      );
      return "failed";
    }
    const url = new URL("/api/v1/sync", location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return new Promise((resolve) => {
      const socket = new WebSocket(url, SUBPROTOCOL);
      socket.binaryType = "arraybuffer";
      this.socket = socket;
      let welcomed = false;
      let heard = Date.now();
      let nonce = 0n;
      let failed: string | null = null;
      const send = (frames: Uint8Array[]) => {
        for (const f of frames) if (socket.readyState === WebSocket.OPEN) socket.send(f as Uint8Array<ArrayBuffer>);
      };
      const ping = setInterval(() => {
        if (Date.now() - heard > SILENT_FOR) {
          failed = "the server stopped answering";
          socket.close();
          return;
        }
        nonce += 1n;
        send([WasmCore.ping(nonce)]);
        this.guard(() => send(this.core.syncRetry()));
        this.moveFiles();
      }, PING_EVERY);
      socket.onmessage = (m) => {
        heard = Date.now();
        if (!(m.data instanceof ArrayBuffer)) return;
        const frame = new Uint8Array(m.data);
        try {
          if (!welcomed) {
            const version = WasmCore.welcome(frame);
            welcomed = true;
            this.status = { ...this.status, serverVersion: version.trim() || null };
            this.setStatus("online");
            send(this.core.syncStart(join === "own" ? undefined : join));
            this.after();
            this.moveFiles();
            return;
          }
          send(this.core.syncReceive(frame));
          this.after();
        } catch (e) {
          failed = e instanceof Error ? e.message : String(e);
          socket.close();
        }
      };
      socket.onclose = (c) => {
        clearInterval(ping);
        if (this.socket === socket) this.socket = null;
        if (this.stopped) return resolve("never");
        try {
          this.core.syncStop();
        } catch {
          // The core is closing.
        }
        if (c.code === SIGNED_OUT_CLOSE) {
          void this.wipe().then(() => this.host.navigate("/"));
          return resolve("never");
        }
        if (failed && welcomed) {
          this.setStatus("failed", failed);
          return resolve("failed");
        }
        this.setStatus("connecting");
        resolve(welcomed ? "online" : "never");
      };
    });
  }

  /** Runs `f` on the core, closing the connection if it fails. */
  private guard(f: () => void): void {
    try {
      f();
      this.after();
    } catch (e) {
      console.warn("drusite: sync", e);
      this.socket?.close();
    }
  }

  private pushChanges(): void {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN || this.status.phase !== "online") return;
    try {
      for (const f of this.core.syncOutgoing()) socket.send(f as Uint8Array<ArrayBuffer>);
    } catch (e) {
      console.warn("drusite: sync", e);
      socket.close();
    }
    this.moveFiles();
  }

  /** Attachments: up what the server lacks, down what the UI asked for. */
  private moveFiles(): void {
    if (this.status.phase !== "online") return;
    const ws = this.workspace;
    const work: [string, boolean][] = [
      ...this.core.attachmentsToUpload().map((b): [string, boolean] => [b, true]),
      ...this.core.attachmentsWanted().map((b): [string, boolean] => [b, false]),
    ];
    for (const [blob, up] of work) {
      if (this.moving.size >= FILES_AT_ONCE) break;
      if (this.moving.has(blob)) continue;
      const bytes = up ? this.core.attachment(blob) : undefined;
      if (up && !bytes) continue;
      this.moving.add(blob);
      const done = (f: () => void) => {
        this.moving.delete(blob);
        if (this.stopped) return;
        this.guard(f);
        this.moveFiles();
      };
      (up ? server.upload(ws, blob, bytes!).then(() => done(() => this.core.attachmentUploaded(blob))) :
        server.download(ws, blob).then((got) =>
          done(() => (got ? this.core.attachmentArrived(blob, got) : this.core.attachmentUnavailable(blob))),
        )
      ).catch((e: unknown) => {
        this.moving.delete(blob);
        console.warn(`drusite: attachment ${blob}`, e);
      });
    }
  }
}

async function shareCall(f: () => Promise<ShareInfo>): Promise<ShareInfo> {
  try {
    return await f();
  } catch (e) {
    throw accountError(new Error(server.shareWords(e)));
  }
}

function isApiError(e: unknown): e is ApiError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e && !(e instanceof Error);
}

function isEmpty(patch: { upserted: unknown[]; removed: unknown[]; order: unknown[] | null }): boolean {
  return patch.upserted.length === 0 && patch.removed.length === 0 && patch.order === null;
}

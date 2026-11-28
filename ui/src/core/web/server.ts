// SPDX-License-Identifier: AGPL-3.0-or-later

// The server the web app came from, over HTTP. The browser holds no token: the session cookie
// (HttpOnly, SameSite=Strict) rides every request, and the `X-Drusite` header makes any request
// from another origin a CORS preflight the server never grants.

import type { AgentCall, AskSource, AskUpdate, Conversation, ConversationSummary, ShareChange, ShareInfo } from "../types";

/** A refused request: the server's error code and its words. */
export class ServerError extends Error {
  constructor(
    readonly status: number,
    readonly code: string | null,
    message: string,
  ) {
    super(message);
    this.name = "ServerError";
  }
}

/** The network failed (offline, or the server is down). */
export class Unreachable extends Error {
  constructor(cause: unknown) {
    super(`can't reach the server: ${cause instanceof Error ? cause.message : String(cause)}`);
    this.name = "Unreachable";
  }
}

const HEADERS = { "X-Drusite": "web" };

async function request(method: string, path: string, body?: unknown, accept = "application/json"): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers: { ...HEADERS, Accept: accept, ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    throw new Unreachable(e);
  }
  if (!response.ok) {
    let code: string | null = null;
    let message = `the server answered ${response.status}`;
    try {
      const j = (await response.json()) as { error?: string; message?: string; error_description?: string };
      code = j.error ?? null;
      message = j.message ?? j.error_description ?? message;
    } catch {
      // Not JSON: the status says enough.
    }
    throw new ServerError(response.status, code ?? String(response.status), message);
  }
  return response;
}

async function json<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await request(method, path, body);
  const text = await r.text();
  return (text ? JSON.parse(text) : null) as T;
}

/** Who this browser is signed in as. */
export interface Session {
  email: string | null;
  /** The server's public address, as it names itself. */
  server: string;
  serverVersion: string | null;
}

/**
 * The session, or why there's none: `signedOut` (the server ended it: signed out from another
 * device, so this browser's copy goes), `none` (never signed in, or it ran out), or `noServer`
 * (not a Drusite server: the dev server, say). Throws `Unreachable` when offline.
 */
export async function session(): Promise<Session | "signedOut" | "none" | "noServer"> {
  try {
    const r = await request("GET", "/api/v1/web/session");
    if (!(r.headers.get("content-type") ?? "").includes("json")) return "noServer";
    return (await r.json()) as Session;
  } catch (e) {
    if (e instanceof ServerError) {
      if (e.status === 401) return e.code === "signed_out" ? "signedOut" : "none";
      if (e.status === 404 || e.status === 405) return "noServer";
    }
    throw e;
  }
}

export async function logout(): Promise<void> {
  await json("POST", "/api/v1/web/logout");
}

export interface WorkspaceInfo {
  id: string;
  role: string;
}

export function workspaces(): Promise<WorkspaceInfo[]> {
  return json("GET", "/api/v1/workspaces");
}

const ws = (workspace: string, rest: string) => `/api/v1/workspaces/${workspace}/${rest}`;

// ---- Ask your notes ----

interface ServerStatus {
  available: boolean;
  reason: string | null;
  message: string | null;
  model: string | null;
  semantic: boolean;
  embeddingModel?: string | null;
  indexing: number;
  pageScope?: boolean;
}

export async function askStatus(workspace: string): Promise<ServerStatus> {
  return json("GET", `/api/v1/ask?workspace=${encodeURIComponent(workspace)}`);
}

interface ServerMessage {
  id: string;
  role: string;
  text: string;
  sources: AskSource[];
  model: string | null;
  error: string | null;
  at: number;
}

export async function conversations(workspace: string): Promise<ConversationSummary[]> {
  return json("GET", ws(workspace, "conversations"));
}

export async function conversation(workspace: string, id: string): Promise<Conversation> {
  const c = await json<{ id: string; title: string; messages: ServerMessage[] }>("GET", ws(workspace, `conversations/${id}`));
  return {
    id: c.id,
    title: c.title,
    messages: c.messages.map((m) => ({
      id: m.id,
      role: m.role === "user" ? "user" : "assistant",
      text: m.text,
      sources: m.sources.map((s) => ({ n: s.n, page: s.page, block: s.block ?? null, title: s.title ?? "", missing: s.missing ?? false })),
      model: m.model,
      error: m.error,
      at: m.at,
    })),
  };
}

export async function renameConversation(workspace: string, id: string, title: string): Promise<void> {
  await json("PATCH", ws(workspace, `conversations/${id}`), { title });
}

export async function deleteConversation(workspace: string, id: string): Promise<void> {
  await json("DELETE", ws(workspace, `conversations/${id}`));
}

/** One server-sent event of an answer as an update, and whether it was the last. */
export function answerEvent(name: string, data: string): [AskUpdate | null, boolean] {
  let v: Record<string, unknown>;
  try {
    v = JSON.parse(data) as Record<string, unknown>;
  } catch {
    return [null, false];
  }
  const text = (k: string) => (typeof v[k] === "string" ? (v[k] as string) : "");
  switch (name) {
    case "started":
      return [{ type: "started", conversation: text("conversation"), title: text("title"), message: text("message") }, false];
    case "sources": {
      const list = Array.isArray(v) ? (v as unknown as AskSource[]) : [];
      return [{ type: "sources", sources: list.map((s) => ({ n: s.n, page: s.page, block: s.block ?? null, title: s.title ?? "", missing: s.missing ?? false })) }, false];
    }
    case "text":
      return [{ type: "text", text: text("text") }, false];
    case "done":
      return [
        {
          type: "done",
          message: typeof v.message === "string" ? v.message : null,
          cited: Array.isArray(v.cited) ? (v.cited as unknown[]).filter((n): n is number => typeof n === "number") : [],
          model: typeof v.model === "string" ? v.model : null,
        },
        true,
      ];
    case "error":
      return [{ type: "failed", message: text("message") }, true];
    default:
      return [null, false];
  }
}

/**
 * Asks, and reads the answer's server-sent events into `onUpdate` until it ends, fails or
 * `signal` stops it. Resolves once the server has accepted the question; the answer goes on.
 */
export async function ask(workspace: string, body: unknown, signal: AbortSignal, onUpdate: (u: AskUpdate) => void): Promise<void> {
  let response: Response;
  try {
    response = await fetch(ws(workspace, "ask"), {
      method: "POST",
      credentials: "same-origin",
      headers: { ...HEADERS, Accept: "text/event-stream", "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
  } catch (e) {
    throw new Unreachable(e);
  }
  if (!response.ok) {
    let message = `the server answered ${response.status}`;
    try {
      const j = (await response.json()) as { message?: string; error_description?: string };
      message = j.message ?? j.error_description ?? message;
    } catch {
      // keep the status
    }
    throw new ServerError(response.status, null, message);
  }
  void (async () => {
    const reader = response.body!.pipeThrough(new TextDecoderStream()).getReader();
    let buffer = "";
    let name = "";
    let data = "";
    let ended = false;
    try {
      while (!ended) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += value;
        let at: number;
        while ((at = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, at).replace(/\r$/, "");
          buffer = buffer.slice(at + 1);
          if (line === "") {
            if (name) {
              const [update, last] = answerEvent(name, data);
              if (update) onUpdate(update);
              ended ||= last;
            }
            name = "";
            data = "";
            if (ended) break;
          } else if (line.startsWith("event:")) name = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).replace(/^ /, "");
        }
      }
      if (!ended) onUpdate({ type: "failed", message: "the answer stopped partway; it may still be saved in the conversation" });
    } catch (e) {
      if (!signal.aborted) onUpdate({ type: "failed", message: `the answer stopped: ${e instanceof Error ? e.message : String(e)}` });
    } finally {
      reader.releaseLock();
    }
  })();
}

// ---- Publish to web ----

interface ServerShare {
  published: boolean;
  url: string | null;
  includeSubpages: boolean;
  allowIndexing: boolean;
  expiresAt: number | null;
  expired: boolean;
  hasPassword: boolean;
  canPublish: boolean;
}

const shareInfo = (s: ServerShare): ShareInfo => ({ ...s, available: true, message: null });

/** A change as the server takes it: absent keeps, null removes. */
export function shareBody(change: ShareChange): Record<string, unknown> {
  const m: Record<string, unknown> = {};
  if (change.includeSubpages != null) m.includeSubpages = change.includeSubpages;
  if (change.allowIndexing != null) m.allowIndexing = change.allowIndexing;
  if (change.expiry) m.expiresAt = change.expiry.type === "set" ? change.expiry.at : null;
  if (change.password) m.password = change.password.type === "set" ? change.password.password : null;
  return m;
}

/** Words for the person when a call fails. */
export function shareWords(e: unknown): string {
  if (e instanceof ServerError && (e.status === 404 || e.code === "not_found"))
    return "This server doesn't know this page, or is older than publishing; it may need updating.";
  return e instanceof Error ? e.message : String(e);
}

const sharePath = (workspace: string, page: string, rest = "") => ws(workspace, `pages/${page}/share${rest}`);

export async function share(workspace: string, page: string): Promise<ShareInfo> {
  return shareInfo(await json("GET", sharePath(workspace, page)));
}

export async function publish(workspace: string, page: string, change: ShareChange): Promise<ShareInfo> {
  return shareInfo(await json("PUT", sharePath(workspace, page), shareBody(change)));
}

export async function unpublish(workspace: string, page: string): Promise<ShareInfo> {
  return shareInfo(await json("DELETE", sharePath(workspace, page)));
}

export async function regenerateShareLink(workspace: string, page: string): Promise<ShareInfo> {
  return shareInfo(await json("POST", sharePath(workspace, page, "/regenerate")));
}

// ---- Agents ----

export async function agentActivity(workspace: string): Promise<AgentCall[]> {
  const items = await json<{ id: string; agent: string; tool: string; summary: string; at: number; undoneAt: number | null; pages: string[] }[]>(
    "GET",
    ws(workspace, "agent-activity"),
  );
  return items.map((i) => ({ id: i.id, source: "server", agent: i.agent, tool: i.tool, summary: i.summary, at: i.at, undoneAt: i.undoneAt, pages: i.pages }));
}

export async function undoAgentCall(workspace: string, id: string): Promise<void> {
  await json("POST", ws(workspace, `agent-activity/${id}/undo`));
}

// ---- Attachments ----

/** Sends a file the server doesn't have. */
export async function upload(workspace: string, blob: string, bytes: Uint8Array): Promise<void> {
  const r = await fetch(ws(workspace, `attachments/${blob}`), {
    method: "PUT",
    credentials: "same-origin",
    headers: { ...HEADERS, "Content-Type": "application/octet-stream" },
    body: bytes as Uint8Array<ArrayBuffer>,
  }).catch((e: unknown) => {
    throw new Unreachable(e);
  });
  if (!r.ok) throw new ServerError(r.status, null, `the server answered ${r.status}`);
}

/** A file from the server, or null when it doesn't have it (or won't say). */
export async function download(workspace: string, blob: string): Promise<Uint8Array | null> {
  const r = await fetch(ws(workspace, `attachments/${blob}`), { credentials: "same-origin", headers: HEADERS }).catch((e: unknown) => {
    throw new Unreachable(e);
  });
  if (r.status === 403 || r.status === 404) return null;
  if (!r.ok) throw new ServerError(r.status, null, `the server answered ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
}

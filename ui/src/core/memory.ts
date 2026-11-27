// SPDX-License-Identifier: AGPL-3.0-or-later

// An in-memory stand-in for the core, with the same commands and the same edit semantics, for
// tests and for working on the UI in a plain browser (`npm run dev`). Nothing is saved.

import { DATE } from "../journal/mood";
import { runQuery } from "../journal/query";
import { dayTitle } from "../journal/dates";
import { MemDatabases } from "./memory-db";
import type { Transport } from "./client";
import type {
  AgentToken,
  AiChoice,
  AiFeature,
  AiKindInfo,
  AiProvider,
  ApiError,
  AskMessage,
  AskSource,
  AskUpdate,
  Block,
  BlockId,
  BlockKind,
  Command,
  Conversation,
  Edit,
  Event,
  Marks,
  PageId,
  PageInfo,
  PageKind,
  PagePatch,
  Property,
  Response,
  SearchHit,
  Settings,
  ShareInfo,
  Span,
  TrashItem,
} from "./types";

type MarkKey = "bold" | "italic" | "underline" | "strikethrough" | "code" | "link";
type MarkSet = Partial<Record<MarkKey, string | true>>;
const KEYS: MarkKey[] = ["bold", "italic", "underline", "strikethrough", "code", "link"];
const GROWS = new Set<MarkKey>(["bold", "italic", "underline", "strikethrough"]);

interface MemBlock {
  id: BlockId;
  kind: BlockKind;
  /** The indent asked for; the shown one is never more than one deeper than the block before. */
  indent: number;
  text: string;
  marks: MarkSet[];
}

interface MemPage {
  id: PageId;
  parent: PageId | null;
  title: string;
  children: PageId[];
  blocks: MemBlock[];
  kind: PageKind;
  properties: Property[];
  /** When it went in the trash (Unix ms), if it did. */
  trashedAt?: number;
}

/** How long the trash keeps a page, as in the core. */
const KEEP_MS = 30 * 24 * 60 * 60 * 1000;

let counter = 0;
function newId(): string {
  counter += 1;
  return `00000000-0000-7000-8000-${counter.toString(16).padStart(12, "0")}`;
}

function fail(code: ApiError["code"], message: string): never {
  throw { code, message } satisfies ApiError;
}

function sameSet(a: MarkSet, b: MarkSet): boolean {
  return KEYS.every((k) => a[k] === b[k]);
}

function toMarks(set: MarkSet): Marks {
  return {
    bold: set.bold === true,
    italic: set.italic === true,
    underline: set.underline === true,
    strikethrough: set.strikethrough === true,
    code: set.code === true,
    link: typeof set.link === "string" ? set.link : null,
  };
}

function spans(b: MemBlock): Span[] {
  const out: Span[] = [];
  for (let i = 0; i < b.text.length; ) {
    let j = i + 1;
    while (j < b.text.length && sameSet(b.marks[j]!, b.marks[i]!)) j++;
    out.push({ text: b.text.slice(i, j), marks: toMarks(b.marks[i]!) });
    i = j;
  }
  return out;
}

function checkPos(b: MemBlock, pos: number): void {
  if (pos > b.text.length) fail("invalidPosition", `position ${pos} is past the end of the block`);
  const c = b.text.charCodeAt(pos - 1);
  if (pos > 0 && pos < b.text.length && c >= 0xd800 && c <= 0xdbff) fail("invalidPosition", `position ${pos} is inside a character`);
}

/** The kinds of provider, as the native core describes them. */
const AI_KINDS: AiKindInfo[] = [
  { kind: "anthropic", label: "Anthropic", defaultBaseUrl: "https://api.anthropic.com/v1", needsKey: true, embeds: false },
  { kind: "openAi", label: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", needsKey: true, embeds: true },
  { kind: "google", label: "Google Gemini", defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta", needsKey: true, embeds: true },
  { kind: "mistral", label: "Mistral", defaultBaseUrl: "https://api.mistral.ai/v1", needsKey: true, embeds: true },
  { kind: "openRouter", label: "OpenRouter", defaultBaseUrl: "https://openrouter.ai/api/v1", needsKey: true, embeds: true },
  { kind: "openAiCompatible", label: "OpenAI-compatible", defaultBaseUrl: null, needsKey: false, embeds: true },
];

export class MemoryCore {
  private pages: MemPage[] = [];
  /** How many views have each page open, as the core counts them. */
  private readonly open = new Map<PageId, number>();
  /** Attachments, in base64, by ID (here a counter, not a hash: nothing checks it). */
  private readonly files = new Map<string, string>();
  private readonly listeners = new Set<(e: Event) => void>();
  /** Agents on "this computer": a stand-in, since nothing here can listen on a port. */
  private localAgents = { enabled: false, port: 47321 };
  private agentTokens: AgentToken[] = [];
  /** AI providers: kept, but never reached (nothing here makes requests). */
  private aiProviders: (AiProvider & { key: string | null })[] = [];
  private aiFeatures = new Map<AiFeature, AiChoice>();
  private readonly dbs = new MemDatabases({
    title: (id) => this.page(id).title,
    rows: (db) => this.page(db).children.filter((c) => !this.trashed(c)),
  });

  readonly transport: Transport = {
    call: async (command) => this.handle(command),
    subscribe: (handler) => {
      this.listeners.add(handler);
      return () => this.listeners.delete(handler);
    },
  };

  /** Pages shared to the web, as the server would keep them. */
  private readonly shares = new Map<
    PageId,
    { token: string; includeSubpages: boolean; allowIndexing: boolean; expiresAt: number | null; password: string | null }
  >();

  private shareInfo(id: PageId): ShareInfo {
    const s = this.shares.get(id);
    return {
      published: !!s,
      url: s ? `https://pages.example.test/p/${s.token}` : null,
      includeSubpages: s?.includeSubpages ?? false,
      allowIndexing: s?.allowIndexing ?? false,
      expiresAt: s?.expiresAt ?? null,
      expired: !!s?.expiresAt && s.expiresAt <= Date.now(),
      hasPassword: !!s?.password,
      canPublish: true,
      available: true,
      message: null,
    };
  }

  private newToken(): string {
    const abc = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    return Array.from({ length: 43 }, () => abc[Math.floor(Math.random() * abc.length)]).join("");
  }

  private page(id: PageId): MemPage {
    return this.pages.find((p) => p.id === id) ?? fail("unknownPage", `no page ${id}`);
  }

  private info(): PageInfo[] {
    const out: PageInfo[] = [];
    const shown = (id: PageId) => this.page(id).trashedAt === undefined;
    const walk = (p: MemPage) => {
      out.push({
        id: p.id,
        parent: p.parent,
        title: p.title,
        children: p.children.filter(shown),
        kind: p.kind,
        properties: [...p.properties].sort((a, b) => (a.name < b.name ? -1 : 1)),
        published: this.shares.has(p.id),
      });
      for (const c of p.children.filter(shown)) walk(this.page(c));
    };
    for (const p of this.pages.filter((p) => p.parent === null && shown(p.id))) walk(p);
    return out;
  }

  /** Whether a page is in the trash, itself or under one that is. */
  private trashed(id: PageId): boolean {
    for (let at: PageId | null = id; at !== null; at = this.page(at).parent) {
      if (this.page(at).trashedAt !== undefined) return true;
    }
    return false;
  }

  private subtree(id: PageId): PageId[] {
    return [id, ...this.page(id).children.flatMap((c) => this.subtree(c))];
  }

  private trash(): TrashItem[] {
    return this.pages
      .filter((p) => p.trashedAt !== undefined)
      .map((p) => ({
        id: p.id,
        title: p.title,
        parent: p.parent,
        trashedAt: p.trashedAt!,
        deletedAt: p.trashedAt! + KEEP_MS,
        subpages: this.subtree(p.id).length - 1,
      }))
      .sort((a, b) => b.trashedAt - a.trashedAt);
  }

  private deleteForever(ids: PageId[]): void {
    const gone = new Set(ids.flatMap((id) => this.subtree(id)));
    for (const p of this.pages) p.children = p.children.filter((c) => !gone.has(c));
    this.pages = this.pages.filter((p) => !gone.has(p.id));
    for (const id of gone) this.open.delete(id);
    this.workspaceChanged();
  }

  private settings: Settings = { journalParent: null, journalTemplate: null, weekStart: 1, askJournal: false };

  /**
   * Ask your notes, faked: the blocks sharing the most words with the question, quoted and
   * cited, streamed in pieces. No model is reached. Conversations live as long as the page.
   */
  private conversations: Conversation[] = [];
  private conversationTimes = new Map<string, number>();
  private asking = new Set<string>();

  private askAnswer(question: string): { sources: AskSource[]; text: string } {
    const words = (t: string) =>
      t
        .normalize("NFD")
        .replace(/\p{M}/gu, "")
        .toLowerCase()
        .split(/[^\p{L}\p{N}]+/u)
        .filter((w) => w.length > 3);
    const want = new Set(words(question));
    const scored: { page: MemPage; block: MemBlock; score: number }[] = [];
    for (const p of this.pages) {
      if (this.trashed(p.id) || (p.kind === "journal" && !this.settings.askJournal)) continue;
      for (const b of p.blocks) {
        const score = words(b.text).filter((w) => want.has(w)).length;
        if (score > 0) scored.push({ page: p, block: b, score });
      }
    }
    scored.sort((a, b) => b.score - a.score);
    const sources = scored.slice(0, 6).map((s, i) => ({
      n: i + 1,
      page: s.page.id,
      block: s.block.id,
      title: s.page.title,
      missing: false,
    }));
    const best = scored[0];
    const text = best ? `From your notes: ${best.block.text} [1]` : "I couldn't find that in your notes.";
    return { sources, text };
  }

  private ask(request: string, conversationId: string | null, question: string): void {
    let conversation = conversationId ? this.conversations.find((c) => c.id === conversationId) : undefined;
    if (conversationId && !conversation) fail("account", "not found");
    if (!conversation) {
      conversation = { id: newId(), title: question.trim().slice(0, 60), messages: [] };
      this.conversations.push(conversation);
    }
    const c = conversation;
    const asked: AskMessage = { id: newId(), role: "user", text: question.trim(), sources: [], model: null, error: null, at: Date.now() };
    c.messages.push(asked);
    this.conversationTimes.set(c.id, Date.now());
    const { sources, text } = this.askAnswer(question);
    const send = (update: AskUpdate) => this.emit({ type: "ask", request, update });
    this.asking.add(request);
    const pieces = text.match(/.{1,12}/gsu) ?? [];
    let i = 0;
    const step = () => {
      if (!this.asking.has(request)) return;
      if (i === 0) {
        send({ type: "started", conversation: c.id, title: c.title, message: asked.id });
        send({ type: "sources", sources });
      }
      if (i < pieces.length) {
        send({ type: "text", text: pieces[i]! });
        i += 1;
        setTimeout(step, 30);
        return;
      }
      const cited = text.includes("[1]") && sources.length > 0 ? [1] : [];
      const answer: AskMessage = {
        id: newId(),
        role: "assistant",
        text,
        sources: sources.filter((s) => cited.includes(s.n)),
        model: "Memory",
        error: null,
        at: Date.now(),
      };
      c.messages.push(answer);
      this.conversationTimes.set(c.id, Date.now());
      this.asking.delete(request);
      send({ type: "done", message: answer.id, cited, model: "Memory" });
    };
    setTimeout(step, 30);
  }

  private currentSettings(): Settings {
    const exists = (id: PageId | null) =>
      id !== null && this.pages.some((p) => p.id === id) && !this.trashed(id) ? id : null;
    return {
      ...this.settings,
      journalParent: exists(this.settings.journalParent),
      journalTemplate: exists(this.settings.journalTemplate),
    };
  }

  private workspaceChanged(): void {
    this.emit({ type: "pagesChanged", pages: this.info() });
    this.emit({ type: "settingsChanged", settings: this.currentSettings() });
    this.emit({ type: "trashChanged", pages: this.trash() });
  }

  private addPage(parent: PageId | null, title: string, kind: PageKind, properties: Property[]): MemPage {
    const page: MemPage = {
      id: newId(),
      parent,
      title,
      children: [],
      blocks: [{ id: newId(), kind: { type: "paragraph" }, indent: 0, text: "", marks: [] }],
      kind,
      properties,
    };
    if (parent !== null) this.page(parent).children.push(page.id);
    this.pages.push(page);
    return page;
  }

  private journalDay(date: string): { page: PageInfo; created: boolean } {
    const existing = this.info().find(
      (p) => p.kind === "journal" && p.properties.some((x) => x.name === DATE && x.value.type === "date" && x.value.value === date),
    );
    if (existing) return { page: existing, created: false };
    let parent = this.currentSettings().journalParent;
    if (parent === null) {
      parent = this.addPage(null, "Journal", "page", []).id;
      this.settings.journalParent = parent;
    }
    const page = this.addPage(parent, dayTitle(date), "journal", [{ name: DATE, value: { type: "date", value: date } }]);
    const template = this.currentSettings().journalTemplate;
    if (template !== null) {
      const fill = (t: string) =>
        t.replaceAll("{{date}}", date).replaceAll("{{title}}", dayTitle(date)).replaceAll("{{weekday}}", dayTitle(date).split(",")[0]!);
      page.blocks = this.page(template).blocks.map((b) => {
        const text = fill(b.text);
        return { id: newId(), kind: b.kind, indent: b.indent, text, marks: text.split("").map((_, i) => b.marks[Math.min(i, b.marks.length - 1)] ?? {}) };
      });
    }
    this.workspaceChanged();
    return { page: this.info().find((p) => p.id === page.id)!, created: true };
  }

  /** A plain stand-in for the core's index: every word must start a word in the text. */
  private search(query: string, limit: number): SearchHit[] {
    const fold = (t: string) => t.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
    const words = fold(query).split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (words.length === 0) return [];
    const matches = (text: string) => {
      const have = fold(text).split(/[^\p{L}\p{N}]+/u);
      return words.every((w, i) => have.some((h) => (i === words.length - 1 ? h.startsWith(w) : h === w)));
    };
    const hits: SearchHit[] = [];
    for (const p of this.info()) {
      const block = this.page(p.id).blocks.find((b) => matches(b.text));
      if (block) hits.push({ page: p.id, title: p.title, block: block.id, snippet: [{ text: block.text, highlight: false }] });
      else if (matches(p.title)) hits.push({ page: p.id, title: p.title, block: null, snippet: [{ text: p.title, highlight: false }] });
      if (hits.length === limit) break;
    }
    return hits;
  }

  private emit(event: Event): void {
    for (const l of this.listeners) l(event);
  }

  /** The blocks of a page as the core would show them. */
  blocks(page: PageId): Block[] {
    const p = this.page(page);
    const shown = this.shownIndents(p);
    return p.blocks.map((b, i) => ({ id: b.id, kind: b.kind, indent: shown[i]!, spans: spans(b) }));
  }

  /** The pages some view has open. */
  openPages(): PageId[] {
    return [...this.open.keys()];
  }

  private shownIndents(p: MemPage): number[] {
    const out: number[] = [];
    p.blocks.forEach((b, i) => out.push(i === 0 ? 0 : Math.min(b.indent, out[i - 1]! + 1)));
    return out;
  }

  async handle(command: Command): Promise<Response> {
    switch (command.type) {
      case "listPages":
        return { type: "pages", pages: this.info() };
      case "createPage": {
        const id = newId();
        const page: MemPage = {
          id,
          parent: command.parent,
          title: command.title,
          children: [],
          blocks: [{ id: newId(), kind: { type: "paragraph" }, indent: 0, text: "", marks: [] }],
          kind: "page",
          properties: [],
        };
        if (command.parent !== null) this.page(command.parent).children.push(id);
        this.pages.push(page);
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "pageCreated", page: this.info().find((p) => p.id === id)! };
      }
      case "renamePage":
        this.page(command.page).title = command.title;
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "done" };
      case "movePage": {
        const p = this.page(command.page);
        for (let a: PageId | null = command.parent; a !== null; a = this.page(a).parent) {
          if (a === p.id) fail("cycle", "a page can't move under itself");
        }
        const siblings = (parent: PageId | null) =>
          parent === null ? this.pages.filter((x) => x.parent === null).map((x) => x.id) : this.page(parent).children;
        if (p.parent !== null) this.page(p.parent).children = this.page(p.parent).children.filter((c) => c !== p.id);
        p.parent = command.parent;
        if (command.parent !== null) {
          const kids = this.page(command.parent).children;
          kids.splice(Math.min(command.index, kids.length), 0, p.id);
        } else {
          const roots = siblings(null).filter((r) => r !== p.id);
          const others = this.pages.filter((x) => x.id !== p.id);
          const anchor = roots[command.index];
          const at = anchor === undefined ? others.length : others.findIndex((x) => x.id === anchor);
          others.splice(at, 0, p);
          this.pages = others;
        }
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "done" };
      }
      case "deletePage": {
        const p = this.page(command.page);
        p.trashedAt ??= Date.now();
        this.workspaceChanged();
        return { type: "done" };
      }
      case "trash":
        return { type: "trash", pages: this.trash() };
      case "restorePage": {
        const p = this.page(command.page);
        delete p.trashedAt;
        if (p.parent !== null && this.trashed(p.parent)) {
          this.page(p.parent).children = this.page(p.parent).children.filter((c) => c !== p.id);
          p.parent = null;
          this.pages = [...this.pages.filter((x) => x.id !== p.id), p];
        }
        this.workspaceChanged();
        return { type: "done" };
      }
      case "deleteForever":
        if (this.page(command.page).trashedAt === undefined) {
          fail("invalid", "only a page in the trash can be deleted for good");
        }
        this.deleteForever([command.page]);
        return { type: "done" };
      case "addAttachment": {
        const blob = newId().replaceAll("-", "").padStart(64, "0");
        this.files.set(blob, command.data);
        const name = command.name.split(/[\\/]/).pop() || "file";
        const mime = command.mime || "application/octet-stream";
        return { type: "attachment", kind: { type: "attachment", blob, name, mime, size: atob(command.data).length } };
      }
      case "readAttachment": {
        const data = this.files.get(command.blob);
        if (data === undefined) fail("notHere", "the file is on its way from the server");
        return { type: "attachmentData", data };
      }
      case "bridgeStatus":
      case "setBridgeFolder":
      case "importFolder":
        fail("unsupported", "the Markdown folder is in the desktop app");
      case "agents":
        return {
          type: "agents",
          local: {
            available: true,
            ...this.localAgents,
            url: `http://127.0.0.1:${this.localAgents.port}/mcp`,
            problem: null,
          },
          tokens: this.agentTokens,
          activity: [],
          serverProblem: null,
        };
      case "setLocalAgents":
        this.localAgents = { enabled: command.enabled, port: command.port };
        this.emit({ type: "agentsChanged" });
        return { type: "done" };
      case "createAgentToken": {
        const name = command.name.trim();
        if (!name) fail("invalid", "give the agent a name of up to 100 characters");
        this.agentTokens = [
          ...this.agentTokens,
          {
            id: newId(),
            name,
            access: { write: command.access.write, journal: command.access.write && command.access.journal },
            createdAt: Date.now(),
            lastUsedAt: null,
          },
        ];
        this.emit({ type: "agentsChanged" });
        return { type: "agentToken", token: `drusite_local_${newId()}`, url: `http://127.0.0.1:${this.localAgents.port}/mcp` };
      }
      case "revokeAgentToken":
        this.agentTokens = this.agentTokens.filter((t) => t.id !== command.id);
        this.emit({ type: "agentsChanged" });
        return { type: "done" };
      case "undoAgentCall":
        fail("invalid", "no such agent call");
      case "ai":
        return {
          type: "ai",
          available: true,
          kinds: AI_KINDS,
          providers: this.aiProviders.map(({ key: _, ...p }) => p),
          features: (["chat", "embeddings"] as const).map((feature) => ({
            feature,
            choice: this.aiFeatures.get(feature) ?? null,
          })),
        };
      case "saveAiProvider": {
        const info = AI_KINDS.find((k) => k.kind === command.kind) ?? fail("invalid", "unknown kind");
        const old = command.id ? this.aiProviders.find((p) => p.id === command.id) : undefined;
        if (command.id && !old) fail("invalid", "no such provider");
        const key = command.apiKey === null ? (old?.key ?? null) : command.apiKey.trim() || null;
        const baseUrl = command.baseUrl?.trim() || null;
        if (baseUrl && !/^https?:\/\/\S+$/.test(baseUrl)) fail("invalid", `“${baseUrl}” isn't an address (http://…)`);
        if (!baseUrl && !info.defaultBaseUrl) fail("invalid", "an OpenAI-compatible endpoint needs its address, such as http://localhost:11434/v1");
        if (info.needsKey && !key) fail("invalid", `${info.label} needs an API key`);
        const provider = {
          id: old?.id ?? newId(),
          kind: command.kind,
          name: command.name.trim() || info.label,
          baseUrl,
          keyHint: key ? `…${key.slice(-4)}` : null,
          key,
        };
        this.aiProviders = old ? this.aiProviders.map((p) => (p.id === old.id ? provider : p)) : [...this.aiProviders, provider];
        if (!info.embeds && this.aiFeatures.get("embeddings")?.provider === provider.id) this.aiFeatures.delete("embeddings");
        this.emit({ type: "aiChanged" });
        const { key: _, ...shown } = provider;
        return { type: "aiProviderSaved", provider: shown };
      }
      case "deleteAiProvider":
        if (!this.aiProviders.some((p) => p.id === command.id)) fail("invalid", "no such provider");
        this.aiProviders = this.aiProviders.filter((p) => p.id !== command.id);
        for (const [f, c] of this.aiFeatures) if (c.provider === command.id) this.aiFeatures.delete(f);
        this.emit({ type: "aiChanged" });
        return { type: "done" };
      case "setAiFeature": {
        const c = command.choice;
        if (c) {
          const p = this.aiProviders.find((x) => x.id === c.provider) ?? fail("invalid", "no such provider");
          const info = AI_KINDS.find((k) => k.kind === p.kind);
          if (command.feature === "embeddings" && !info?.embeds) fail("invalid", `${info?.label} doesn't make embeddings`);
          if (!c.model.trim()) fail("invalid", "pick a model");
          this.aiFeatures.set(command.feature, { provider: c.provider, model: c.model.trim() });
        } else {
          this.aiFeatures.delete(command.feature);
        }
        this.emit({ type: "aiChanged" });
        return { type: "done" };
      }
      case "aiModels":
        return { type: "aiModels", models: [] };
      case "testAiProvider":
        return {
          type: "aiCheck",
          steps: [{ what: "Models", ok: false, detail: "This preview can't reach providers", millis: 0 }],
        };
      case "emptyTrash":
        this.deleteForever(this.trash().map((t) => t.id));
        return { type: "done" };
      case "openPage": {
        const p = this.page(command.page);
        this.open.set(p.id, (this.open.get(p.id) ?? 0) + 1);
        return { type: "opened", view: { id: p.id, title: p.title, blocks: this.blocks(p.id) } };
      }
      case "closePage": {
        const views = (this.open.get(command.page) ?? 0) - 1;
        if (views > 0) this.open.set(command.page, views);
        else this.open.delete(command.page);
        return { type: "done" };
      }
      case "accountStatus":
        return {
          type: "account",
          status: { server: null, email: null, phase: "signedOut", message: null },
        };
      case "signIn":
      case "cancelSignIn":
      case "signOut":
        fail("unsupported", "Signing in needs the Drusite app; in a browser, notes aren't saved.");
      case "setProperty": {
        const page = this.page(command.page);
        const name = command.name.trim();
        if (name === "" || /[\u0000-\u001f]/.test(name)) fail("invalid", `not a property name: ${JSON.stringify(name)}`);
        page.properties = page.properties.filter((p) => p.name !== name);
        if (command.value) page.properties.push({ name, value: command.value });
        this.workspaceChanged();
        return { type: "done" };
      }
      case "openJournalDay":
        return { type: "journalDay", ...this.journalDay(command.date) };
      case "queryPages":
        return { type: "pages", pages: runQuery(this.info(), command.query) };
      case "settings":
        return { type: "settings", settings: this.currentSettings() };
      case "setSetting": {
        const s = command.setting;
        if (s.type === "askJournal") {
          this.settings.askJournal = s.value;
        } else if (s.type === "weekStart") {
          if (s.value < 0 || s.value > 6) fail("invalid", `week start ${s.value} isn't a weekday (0-6)`);
          this.settings.weekStart = s.value;
        } else {
          const id = s.value ?? null;
          if (id !== null) this.page(id);
          if (s.type === "journalParent") this.settings.journalParent = id;
          else this.settings.journalTemplate = id;
        }
        this.workspaceChanged();
        return { type: "settings", settings: this.currentSettings() };
      }
      case "search":
        return { type: "searchResults", hits: this.search(command.query, command.limit) };
      case "askStatus":
        return {
          type: "askStatus",
          status: { signedIn: true, available: true, message: null, reason: null, model: "Memory", semantic: false, indexing: 0 },
        };
      case "conversations":
        return {
          type: "conversations",
          conversations: this.conversations
            .map((c) => ({ id: c.id, title: c.title, updatedAt: this.conversationTimes.get(c.id) ?? 0 }))
            .sort((a, b) => b.updatedAt - a.updatedAt),
        };
      case "conversation": {
        const c = this.conversations.find((x) => x.id === command.id);
        if (!c) fail("account", "not found");
        const titles = new Map(this.pages.map((p) => [p.id, p.title]));
        return {
          type: "conversation",
          conversation: {
            ...c,
            messages: c.messages.map((m) => ({
              ...m,
              sources: m.sources.map((s) => ({ ...s, title: titles.get(s.page) ?? "", missing: !titles.has(s.page) || this.trashed(s.page) })),
            })),
          },
        };
      }
      case "renameConversation": {
        const c = this.conversations.find((x) => x.id === command.id);
        if (!c) fail("account", "not found");
        const title = command.title.trim();
        if (title === "") fail("account", "a title has 1 to 120 characters");
        c.title = title;
        return { type: "done" };
      }
      case "deleteConversation":
        this.conversations = this.conversations.filter((c) => c.id !== command.id);
        return { type: "done" };
      case "ask":
        this.ask(command.request, command.conversation, command.question);
        return { type: "done" };
      case "cancelAsk":
        this.asking.delete(command.request);
        return { type: "done" };
      case "share":
      case "localShare":
        this.page(command.page);
        return { type: "share", share: this.shareInfo(command.page) };
      case "publish": {
        if (this.trashed(command.page)) fail("account", "this page is in the trash; restore it to publish it");
        const c = command.change;
        if (c.expiry?.type === "set" && c.expiry.at <= Date.now()) fail("account", "the expiry date must be in the future");
        if (c.password?.type === "set" && c.password.password.length < 4)
          fail("account", "a page's password needs 4 to 256 characters");
        const s = this.shares.get(command.page) ?? {
          token: this.newToken(),
          includeSubpages: false,
          allowIndexing: false,
          expiresAt: null,
          password: null,
        };
        if (c.includeSubpages !== null) s.includeSubpages = c.includeSubpages;
        if (c.allowIndexing !== null) s.allowIndexing = c.allowIndexing;
        if (c.expiry) s.expiresAt = c.expiry.type === "set" ? c.expiry.at : null;
        if (c.password) s.password = c.password.type === "set" ? c.password.password : null;
        this.shares.set(command.page, s);
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "share", share: this.shareInfo(command.page) };
      }
      case "unpublish":
        this.shares.delete(command.page);
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "share", share: this.shareInfo(command.page) };
      case "regenerateShareLink": {
        const s = this.shares.get(command.page) ?? fail("account", "this page isn't published");
        s.token = this.newToken();
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "share", share: this.shareInfo(command.page) };
      }
      case "ready":
        return { type: "done" };
      case "createDatabase": {
        const page = this.addPage(command.parent, command.title, "database", []);
        this.dbs.create(page.id, command.inline, command.template);
        this.workspaceChanged();
        return { type: "databaseCreated", page: this.info().find((p) => p.id === page.id)!, schema: this.dbs.schema(page.id) };
      }
      case "database":
        return { type: "database", schema: this.dbs.schema(command.database) };
      case "queryView":
        return { type: "view", result: this.dbs.query(command.database, command.view) };
      case "rowCells": {
        const parent = this.page(command.row).parent;
        if (parent === null || !this.dbs.isDatabase(parent)) return { type: "rowCells", schema: null, row: null };
        return { type: "rowCells", schema: this.dbs.schema(parent), row: this.dbs.row(parent, command.row) };
      }
      case "putProperty": {
        const schema = this.dbs.putProperty(command.database, command.property);
        this.workspaceChanged();
        return { type: "database", schema };
      }
      case "deleteProperty": {
        const schema = this.dbs.deleteProperty(command.database, command.property);
        this.workspaceChanged();
        return { type: "database", schema };
      }
      case "putView": {
        const schema = this.dbs.putView(command.database, command.view);
        this.workspaceChanged();
        return { type: "database", schema };
      }
      case "deleteView": {
        const schema = this.dbs.deleteView(command.database, command.view);
        this.workspaceChanged();
        return { type: "database", schema };
      }
      case "setCell":
        if (command.property === "title") this.page(command.row).title = command.value?.type === "text" ? command.value.value : "";
        else this.dbs.setCell(command.row, command.property, command.value);
        this.workspaceChanged();
        return { type: "done" };
      case "createRow": {
        const page = this.addPage(command.database, command.title, "page", []);
        if (command.after !== null) {
          const kids = this.page(command.database).children;
          kids.splice(kids.indexOf(page.id), 1);
          kids.splice(kids.indexOf(command.after) + 1, 0, page.id);
        }
        for (const c of command.values) this.dbs.setCell(page.id, c.property, c.value);
        this.dbs.rowCreated(command.database, page.id);
        this.workspaceChanged();
        return { type: "pageCreated", page: this.info().find((p) => p.id === page.id)! };
      }
      case "checkFormula":
        return { type: "formulaCheck", check: { result: { type: "any" }, error: null } };
      case "importNotion":
      case "importNotionData":
      case "importNotionApi":
      case "notionToken":
      case "forgetNotionToken":
        fail("unsupported", "importing from Notion needs the Drusite app");
      case "edit":
        // As in the core, only an open page takes edits.
        if (!this.open.has(command.page)) fail("pageNotOpen", `page ${command.page} isn't open`);
        return { type: "edited", outcome: this.edit(command.page, command.edits) };
    }
  }

  private edit(pageId: PageId, edits: Edit[]): { created: BlockId[]; patch: PagePatch } {
    const p = this.page(pageId);
    const before = this.blocks(pageId);
    const touched = new Set<BlockId>();
    const created: BlockId[] = [];
    const find = (id: BlockId) => {
      const i = p.blocks.findIndex((b) => b.id === id);
      if (i < 0) fail("unknownBlock", `no block ${id}`);
      return i;
    };
    for (const e of edits) {
      switch (e.type) {
        case "insertText": {
          const b = p.blocks[find(e.block)]!;
          checkPos(b, e.pos);
          const prev = e.pos > 0 ? b.marks[e.pos - 1]! : {};
          const next = e.pos < b.text.length ? b.marks[e.pos]! : {};
          const inherited: MarkSet = {};
          for (const k of KEYS) if (prev[k] !== undefined && (next[k] === prev[k] || GROWS.has(k))) inherited[k] = prev[k];
          b.text = b.text.slice(0, e.pos) + e.text + b.text.slice(e.pos);
          b.marks.splice(e.pos, 0, ...Array.from({ length: e.text.length }, () => ({ ...inherited })));
          touched.add(b.id);
          break;
        }
        case "deleteText": {
          const b = p.blocks[find(e.block)]!;
          checkPos(b, e.pos);
          checkPos(b, e.pos + e.len);
          b.text = b.text.slice(0, e.pos) + b.text.slice(e.pos + e.len);
          b.marks.splice(e.pos, e.len);
          touched.add(b.id);
          break;
        }
        case "format": {
          const b = p.blocks[find(e.block)]!;
          checkPos(b, e.start);
          checkPos(b, e.end);
          const k = e.mark.type as MarkKey;
          for (let i = e.start; i < e.end; i++) {
            const m = { ...b.marks[i]! };
            if (e.on) m[k] = e.mark.type === "link" ? e.mark.href : true;
            else delete m[k];
            b.marks[i] = m;
          }
          touched.add(b.id);
          break;
        }
        case "split": {
          const i = find(e.block);
          const b = p.blocks[i]!;
          checkPos(b, e.pos);
          const id = newId();
          const indent = this.shownIndents(p)[i]!;
          p.blocks.splice(i + 1, 0, { id, kind: e.kind, indent, text: b.text.slice(e.pos), marks: b.marks.slice(e.pos) });
          b.text = b.text.slice(0, e.pos);
          b.marks = b.marks.slice(0, e.pos);
          created.push(id);
          touched.add(b.id).add(id);
          break;
        }
        case "insertBlock": {
          const i = e.after === null ? -1 : find(e.after);
          const indent = i < 0 ? 0 : this.shownIndents(p)[i]!;
          const id = newId();
          p.blocks.splice(i + 1, 0, { id, kind: e.kind, indent, text: "", marks: [] });
          created.push(id);
          touched.add(id);
          break;
        }
        case "join": {
          const i = find(e.block);
          if (i === 0) break;
          const prev = p.blocks[i - 1]!;
          const b = p.blocks[i]!;
          prev.text += b.text;
          prev.marks = prev.marks.concat(b.marks);
          p.blocks.splice(i, 1);
          touched.add(prev.id);
          break;
        }
        case "deleteBlock":
          // As in the core, even the last block goes; the editor always sends what replaces it
          // in the same batch.
          p.blocks.splice(find(e.block), 1);
          break;
        case "setKind":
          p.blocks[find(e.block)]!.kind = e.kind;
          touched.add(e.block);
          break;
        case "setIndent": {
          const i = find(e.block);
          const limit = i === 0 ? 0 : this.shownIndents(p)[i - 1]! + 1;
          p.blocks[i]!.indent = Math.min(e.indent, limit, 8);
          touched.add(e.block);
          break;
        }
        case "moveBlock": {
          const i = find(e.block);
          if (e.after === e.block) break;
          const [b] = p.blocks.splice(i, 1);
          const at = e.after === null ? 0 : find(e.after) + 1;
          p.blocks.splice(at, 0, b!);
          touched.add(e.block);
          break;
        }
      }
    }
    const after = this.blocks(pageId);
    const old = new Map(before.map((b) => [b.id, b]));
    const upserted = after
      .map((b, i) => ({ b, i }))
      .filter(({ b }) => touched.has(b.id) || old.get(b.id)?.indent !== b.indent)
      .map(({ b, i }) => ({ block: b, after: i > 0 ? after[i - 1]!.id : null }));
    const live = new Set(after.map((b) => b.id));
    return { created, patch: { upserted, removed: before.map((b) => b.id).filter((id) => !live.has(id)), order: null } };
  }
}

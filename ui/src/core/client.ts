// SPDX-License-Identifier: AGPL-3.0-or-later

// The UI's way into the core. Everything in the UI goes through `CoreClient`; how a command
// reaches the core (Tauri's IPC on desktop and mobile, a direct call into WebAssembly in the
// browser) is a `Transport`, chosen once at startup. Nothing else in the UI knows which.

import type {
  AccountStatus,
  AgentAccess,
  AgentSource,
  AiCheckStep,
  AiChoice,
  AiFeature,
  AiKind,
  AiModel,
  AiProvider,
  ApiError,
  AskStatus,
  BlockKind,
  BridgeStatus,
  Command,
  Conversation,
  ConversationSummary,
  Edit,
  EditOutcome,
  ImportReport,
  Event,
  PageId,
  PageInfo,
  PageQuery,
  PageView,
  PropertyValue,
  Response,
  SearchHit,
  Setting,
  Settings,
  ShareChange,
  ShareInfo,
  TrashItem,
  Template,
  Schema,
  ViewResult,
  RowView,
  PropertyDef,
  View,
  Value,
  CellValue,
  FormulaCheck,
  NotionReport,
  Clock,
} from "./types";

/** The person's clock, for formulas and "today" in filters. */
export function clock(): Clock {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return {
    now: `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`,
    utcOffset: -d.getTimezoneOffset(),
  };
}

export type * from "./types";

/** Carries commands to the core and events back. */
export interface Transport {
  call(command: Command): Promise<Response>;
  /** Calls `handler` for every core event until the returned function is called. */
  subscribe(handler: (event: Event) => void): () => void;
  /** Asks the person for a folder, where the platform can (desktops). */
  /** The system's folder picker, titled `title`; desktops only. */
  pickFolder?: (title: string) => Promise<string | null>;
  /** The system's file picker, for files ending in one of `extensions`; desktops only. */
  pickFile?: (title: string, extensions: string[]) => Promise<string | null>;
}

/** A failed command, with the core's error code. */
export class CoreError extends Error {
  readonly code: ApiError["code"];

  constructor(error: ApiError) {
    super(error.message);
    this.name = "CoreError";
    this.code = error.code;
  }
}

function isApiError(value: unknown): value is ApiError {
  return typeof value === "object" && value !== null && "code" in value && "message" in value;
}

export class CoreClient {
  constructor(private readonly transport: Transport) {}

  subscribe(handler: (event: Event) => void): () => void {
    return this.transport.subscribe(handler);
  }

  private async call<T extends Response["type"]>(
    command: Command,
    expected: T,
  ): Promise<Extract<Response, { type: T }>> {
    let response: Response;
    try {
      response = await this.transport.call(command);
    } catch (error) {
      throw isApiError(error) ? new CoreError(error) : error;
    }
    if (response.type !== expected) {
      throw new Error(`core answered ${command.type} with ${response.type}, expected ${expected}`);
    }
    return response as Extract<Response, { type: T }>;
  }

  async listPages(): Promise<PageInfo[]> {
    return (await this.call({ type: "listPages" }, "pages")).pages;
  }

  async createPage(parent: PageId | null, title: string): Promise<PageInfo> {
    return (await this.call({ type: "createPage", parent, title }, "pageCreated")).page;
  }

  async renamePage(page: PageId, title: string): Promise<void> {
    await this.call({ type: "renamePage", page, title }, "done");
  }

  async movePage(page: PageId, parent: PageId | null, index: number): Promise<void> {
    await this.call({ type: "movePage", page, parent, index }, "done");
  }

  async deletePage(page: PageId): Promise<void> {
    await this.call({ type: "deletePage", page }, "done");
  }

  /** What's in the trash, most recent first. */
  async trash(): Promise<TrashItem[]> {
    return (await this.call({ type: "trash" }, "trash")).pages;
  }

  async restorePage(page: PageId): Promise<void> {
    await this.call({ type: "restorePage", page }, "done");
  }

  async deleteForever(page: PageId): Promise<void> {
    await this.call({ type: "deleteForever", page }, "done");
  }

  async emptyTrash(): Promise<void> {
    await this.call({ type: "emptyTrash" }, "done");
  }

  /** Keeps a file (`data` in base64) on the device and returns the block kind for it. */
  async addAttachment(name: string, mime: string, data: string): Promise<BlockKind> {
    return (await this.call({ type: "addAttachment", name, mime, data }, "attachment")).kind;
  }

  /** A file, in base64. Fails with code `notHere` while it's still coming from the server. */
  async readAttachment(blob: string): Promise<string> {
    return (await this.call({ type: "readAttachment", blob }, "attachmentData")).data;
  }

  /** The Markdown bridge, or null where there is none (phones, the browser). */
  async bridgeStatus(): Promise<BridgeStatus | null> {
    try {
      return (await this.call({ type: "bridgeStatus" }, "bridge")).status;
    } catch (e) {
      if (e instanceof CoreError && e.code === "unsupported") return null;
      throw e;
    }
  }

  /** Turns the Markdown bridge on with `folder`, or off. */
  async setBridgeFolder(folder: string | null): Promise<BridgeStatus> {
    return (await this.call({ type: "setBridgeFolder", folder }, "bridge")).status;
  }

  /** Imports a folder of Markdown notes as new pages. */
  async importFolder(folder: string): Promise<ImportReport> {
    return (await this.call({ type: "importFolder", folder }, "imported")).report;
  }

  /** Agents: this computer's endpoint and tokens, and what agents did here and on the server. */
  async agents(): Promise<Omit<Extract<Response, { type: "agents" }>, "type">> {
    const { type: _, ...rest } = await this.call({ type: "agents" }, "agents");
    return rest;
  }

  async setLocalAgents(enabled: boolean, port: number): Promise<void> {
    await this.call({ type: "setLocalAgents", enabled, port }, "done");
  }

  /** A token for an agent on this computer: shown only now. */
  async createAgentToken(name: string, access: AgentAccess): Promise<{ token: string; url: string }> {
    const { token, url } = await this.call({ type: "createAgentToken", name, access }, "agentToken");
    return { token, url };
  }

  async revokeAgentToken(id: string): Promise<void> {
    await this.call({ type: "revokeAgentToken", id }, "done");
  }

  /** Takes back one agent call; whatever changed since stays. */
  async undoAgentCall(id: string, source: AgentSource): Promise<void> {
    await this.call({ type: "undoAgentCall", id, source }, "done");
  }

  /** AI: the kinds of provider there are, the ones set up on this device, and each feature's choice. */
  async ai(): Promise<Omit<Extract<Response, { type: "ai" }>, "type">> {
    const { type: _, ...rest } = await this.call({ type: "ai" }, "ai");
    return rest;
  }

  /** Adds a provider (no `id`) or changes one. A null key keeps the saved one; "" removes it. */
  async saveAiProvider(p: {
    id: string | null;
    kind: AiKind;
    name: string;
    baseUrl: string | null;
    apiKey: string | null;
  }): Promise<AiProvider> {
    return (await this.call({ type: "saveAiProvider", ...p }, "aiProviderSaved")).provider;
  }

  async deleteAiProvider(id: string): Promise<void> {
    await this.call({ type: "deleteAiProvider", id }, "done");
  }

  async setAiFeature(feature: AiFeature, choice: AiChoice | null): Promise<void> {
    await this.call({ type: "setAiFeature", feature, choice }, "done");
  }

  async aiModels(provider: string): Promise<AiModel[]> {
    return (await this.call({ type: "aiModels", provider }, "aiModels")).models;
  }

  /** Test connection: the models, then a tiny request to each model given. */
  async testAiProvider(provider: string, chatModel: string | null, embeddingModel: string | null): Promise<AiCheckStep[]> {
    return (await this.call({ type: "testAiProvider", provider, chatModel, embeddingModel }, "aiCheck")).steps;
  }

  /** Asks for a folder; null if the person cancels or the platform can't. */
  async pickFolder(title: string): Promise<string | null> {
    return (await this.transport.pickFolder?.(title)) ?? null;
  }

  /** Whether this platform can give a file's path (desktops); elsewhere files come as bytes. */
  get canPickFile(): boolean {
    return !!this.transport.pickFile;
  }

  async pickFile(title: string, extensions: string[]): Promise<string | null> {
    return (await this.transport.pickFile?.(title, extensions)) ?? null;
  }

  // ---- Databases ----

  async createDatabase(parent: PageId | null, title: string, inline: boolean, template: Template): Promise<{ page: PageInfo; schema: Schema }> {
    const { page, schema } = await this.call({ type: "createDatabase", parent, title, inline, template }, "databaseCreated");
    return { page, schema };
  }

  async database(database: PageId): Promise<Schema> {
    return (await this.call({ type: "database", database }, "database")).schema;
  }

  async queryView(database: PageId, view: string | null): Promise<ViewResult> {
    return (await this.call({ type: "queryView", database, view, clock: clock() }, "view")).result;
  }

  async rowCells(row: PageId): Promise<{ schema: Schema | null; row: RowView | null }> {
    const { schema, row: r } = await this.call({ type: "rowCells", row, clock: clock() }, "rowCells");
    return { schema, row: r };
  }

  async putProperty(database: PageId, property: PropertyDef, twinName: string | null = null): Promise<Schema> {
    return (await this.call({ type: "putProperty", database, property, twinName }, "database")).schema;
  }

  async deleteProperty(database: PageId, property: string): Promise<Schema> {
    return (await this.call({ type: "deleteProperty", database, property }, "database")).schema;
  }

  async putView(database: PageId, view: View): Promise<Schema> {
    return (await this.call({ type: "putView", database, view }, "database")).schema;
  }

  async deleteView(database: PageId, view: string): Promise<Schema> {
    return (await this.call({ type: "deleteView", database, view }, "database")).schema;
  }

  async setCell(row: PageId, property: string, value: Value | null): Promise<void> {
    await this.call({ type: "setCell", row, property, value }, "done");
  }

  async createRow(database: PageId, title: string, values: CellValue[] = [], after: PageId | null = null): Promise<PageInfo> {
    return (await this.call({ type: "createRow", database, title, values, after }, "pageCreated")).page;
  }

  async checkFormula(database: PageId, expression: string): Promise<FormulaCheck> {
    return (await this.call({ type: "checkFormula", database, expression }, "formulaCheck")).check;
  }

  /** Imports a Notion export: its `.zip` or the folder it unzips to (desktops). */
  async importNotion(path: string): Promise<NotionReport> {
    return (await this.call({ type: "importNotion", path }, "notionImported")).report;
  }

  /** Imports a Notion export from its bytes (base64). */
  async importNotionData(name: string, data: string): Promise<NotionReport> {
    return (await this.call({ type: "importNotionData", name, data }, "notionImported")).report;
  }

  /** Imports from Notion's API; a token given is kept for next time, none uses the kept one. */
  async importNotionApi(token: string | null): Promise<NotionReport> {
    return (await this.call({ type: "importNotionApi", token }, "notionImported")).report;
  }

  async notionToken(): Promise<boolean> {
    return (await this.call({ type: "notionToken" }, "notionToken")).saved;
  }

  async forgetNotionToken(): Promise<void> {
    await this.call({ type: "forgetNotionToken" }, "done");
  }

  async openPage(page: PageId): Promise<PageView> {
    return (await this.call({ type: "openPage", page }, "opened")).view;
  }

  async closePage(page: PageId): Promise<void> {
    await this.call({ type: "closePage", page }, "done");
  }

  async edit(page: PageId, edits: Edit[]): Promise<EditOutcome> {
    return (await this.call({ type: "edit", page, edits }, "edited")).outcome;
  }

  async accountStatus(): Promise<AccountStatus> {
    return (await this.call({ type: "accountStatus" }, "account")).status;
  }

  /** Starts signing in to `server` in the system browser; progress arrives as `accountChanged`. */
  async signIn(server: string): Promise<void> {
    await this.call({ type: "signIn", server }, "done");
  }

  async cancelSignIn(): Promise<void> {
    await this.call({ type: "cancelSignIn" }, "done");
  }

  async signOut(): Promise<void> {
    await this.call({ type: "signOut" }, "done");
  }

  async setProperty(page: PageId, name: string, value: PropertyValue | null): Promise<void> {
    await this.call({ type: "setProperty", page, name, value }, "done");
  }

  /** The journal entry for `date` (YYYY-MM-DD, the device's own day), made if there isn't one. */
  async openJournalDay(date: string): Promise<{ page: PageInfo; created: boolean }> {
    const { page, created } = await this.call({ type: "openJournalDay", date }, "journalDay");
    return { page, created };
  }

  async queryPages(query: PageQuery): Promise<PageInfo[]> {
    return (await this.call({ type: "queryPages", query }, "pages")).pages;
  }

  async settings(): Promise<Settings> {
    return (await this.call({ type: "settings" }, "settings")).settings;
  }

  async setSetting(setting: Setting): Promise<Settings> {
    return (await this.call({ type: "setSetting", setting }, "settings")).settings;
  }

  /** Full-text search of this device's pages, best first, one hit per page. */
  async search(query: string, limit = 20): Promise<SearchHit[]> {
    return (await this.call({ type: "search", query, limit }, "searchResults")).hits;
  }

  /** Whether Ask your notes works with this device's server, and with what. */
  async askStatus(): Promise<AskStatus> {
    return (await this.call({ type: "askStatus" }, "askStatus")).status;
  }

  /** The saved conversations, most recent first. */
  async conversations(): Promise<ConversationSummary[]> {
    return (await this.call({ type: "conversations" }, "conversations")).conversations;
  }

  async conversation(id: string): Promise<Conversation> {
    return (await this.call({ type: "conversation", id }, "conversation")).conversation;
  }

  async renameConversation(id: string, title: string): Promise<void> {
    await this.call({ type: "renameConversation", id, title }, "done");
  }

  async deleteConversation(id: string): Promise<void> {
    await this.call({ type: "deleteConversation", id }, "done");
  }

  /**
   * Asks the notes, or only `page` and the pages inside it. The answer comes as `ask` events
   * naming `request`; this resolves once it's on its way.
   */
  async ask(
    request: string,
    conversation: string | null,
    question: string,
    today: string | null,
    page: PageId | null = null,
  ): Promise<void> {
    await this.call({ type: "ask", request, conversation, question, today, page }, "done");
  }

  /** Stops following an answer; the server still finishes and saves it. */
  async cancelAsk(request: string): Promise<void> {
    await this.call({ type: "cancelAsk", request }, "done");
  }

  /** How a page is shared to the web, from the server (or what the workspace last heard). */
  async share(page: PageId): Promise<ShareInfo> {
    return (await this.call({ type: "share", page }, "share")).share;
  }

  /** Publishes a page to the web, or changes how. */
  async publish(page: PageId, change: ShareChange): Promise<ShareInfo> {
    return (await this.call({ type: "publish", page, change }, "share")).share;
  }

  /** Takes a page off the web at once. */
  async unpublish(page: PageId): Promise<ShareInfo> {
    return (await this.call({ type: "unpublish", page }, "share")).share;
  }

  /** A new link for a published page; the old one stops working. */
  async regenerateShareLink(page: PageId): Promise<ShareInfo> {
    return (await this.call({ type: "regenerateShareLink", page }, "share")).share;
  }

  /** Tells the core the UI is showing what it opened (benchmarks wait for this). */
  async ready(): Promise<void> {
    await this.call({ type: "ready" }, "done");
  }
}

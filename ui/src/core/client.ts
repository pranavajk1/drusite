// SPDX-License-Identifier: AGPL-3.0-or-later

// The UI's way into the core. Everything in the UI goes through `CoreClient`; how a command
// reaches the core (Tauri's IPC on desktop and mobile, a direct call into WebAssembly in the
// browser) is a `Transport`, chosen once at startup. Nothing else in the UI knows which.

import type {
  AccountStatus,
  ApiError,
  BlockKind,
  BridgeStatus,
  Command,
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
  TrashItem,
} from "./types";

export type * from "./types";

/** Carries commands to the core and events back. */
export interface Transport {
  call(command: Command): Promise<Response>;
  /** Calls `handler` for every core event until the returned function is called. */
  subscribe(handler: (event: Event) => void): () => void;
  /** Asks the person for a folder, where the platform can (desktops). */
  pickFolder?: () => Promise<string | null>;
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

  /** Asks for a folder; null if the person cancels or the platform can't. */
  async pickFolder(): Promise<string | null> {
    return (await this.transport.pickFolder?.()) ?? null;
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

  /** Tells the core the UI is showing what it opened (benchmarks wait for this). */
  async ready(): Promise<void> {
    await this.call({ type: "ready" }, "done");
  }
}

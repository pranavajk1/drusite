// SPDX-License-Identifier: AGPL-3.0-or-later

// The UI's way into the core. Everything in the UI goes through `CoreClient`; how a command
// reaches the core (Tauri's IPC on desktop and mobile, a direct call into WebAssembly in the
// browser) is a `Transport`, chosen once at startup. Nothing else in the UI knows which.

import type {
  ApiError,
  Command,
  Edit,
  EditOutcome,
  Event,
  PageId,
  PageInfo,
  PageView,
  Response,
} from "./types";

export type * from "./types";

/** Carries commands to the core and events back. */
export interface Transport {
  call(command: Command): Promise<Response>;
  /** Calls `handler` for every core event until the returned function is called. */
  subscribe(handler: (event: Event) => void): () => void;
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

  async openPage(page: PageId): Promise<PageView> {
    return (await this.call({ type: "openPage", page }, "opened")).view;
  }

  async closePage(page: PageId): Promise<void> {
    await this.call({ type: "closePage", page }, "done");
  }

  async edit(page: PageId, edits: Edit[]): Promise<EditOutcome> {
    return (await this.call({ type: "edit", page, edits }, "edited")).outcome;
  }

  /** Tells the core the UI is showing what it opened (benchmarks wait for this). */
  async ready(): Promise<void> {
    await this.call({ type: "ready" }, "done");
  }
}

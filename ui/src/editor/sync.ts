// SPDX-License-Identifier: AGPL-3.0-or-later

// Keeping the core's copy of the open page in step with what ProseMirror shows.
//
// The page's document lives in the core. ProseMirror applies an edit at once, so typing never
// waits on IPC; the binding then works out which blocks the edit touched (`region.ts`), turns
// the change into the core's edits (`diff.ts`) and queues them. The queue sends one batch at a
// time, in order. Blocks ProseMirror creates get a temporary ID until the core answers with the
// real one; an edit that needs a block's real ID waits for it.

import type { Node } from "prosemirror-model";
import { Plugin, PluginKey, type EditorState, type Transaction } from "prosemirror-state";
import type { EditorView } from "prosemirror-view";

import type { CoreClient } from "../core/client";
import type { Block, Edit, PagePatch, PageView } from "../core/types";
import { attrsOf, docNode } from "./convert";
import { type LocalEdit, diffBlocks } from "./diff";
import { blocksIn, changedRegion } from "./region";
import { MAX_INDENT } from "./schema";

/** Transaction meta for changes that came from the core: never sent back to it. */
export const FROM_CORE = "drusite-from-core";

let tmpCounter = 0;
export function tmpId(): string {
  tmpCounter += 1;
  return `tmp-${tmpCounter}`;
}

const isFromCore = (tr: Transaction) => tr.getMeta(FROM_CORE) === true;

/**
 * Gives every block a usable ID: a block ProseMirror created (Enter, paste, undo of a delete)
 * or copied (whose ID is now on two blocks) gets a fresh temporary one. Also keeps indents from
 * skipping a level, as the core does.
 */
export const idPlugin = new Plugin({
  key: new PluginKey("drusite-ids"),
  appendTransaction(transactions, oldState, newState) {
    if (transactions.every(isFromCore)) return null;
    const steps = transactions.filter((t) => !isFromCore(t)).flatMap((t) => t.steps);
    const region = changedRegion(oldState.doc, newState.doc, steps);
    if (!region) return null;
    const oldIds = new Set(blocksIn(oldState.doc, region.start, region.oldEnd).map((n) => attrsOf(n).id));
    const seen = new Set<string>();
    let tr: Transaction | null = null;
    let pos = 0;
    for (let i = 0; i < region.start; i++) pos += newState.doc.child(i).nodeSize;
    let prevIndent = region.start > 0 ? attrsOf(newState.doc.child(region.start - 1)).indent : -1;
    for (let i = region.start; i < region.newEnd; i++) {
      const node = newState.doc.child(i);
      const { id, indent } = attrsOf(node);
      if (id === null || !oldIds.has(id) || seen.has(id)) {
        tr ??= newState.tr;
        tr.setNodeAttribute(pos, "id", tmpId());
      } else {
        seen.add(id);
      }
      const limit = Math.min(prevIndent + 1, MAX_INDENT);
      const shown = Math.max(0, Math.min(indent, limit));
      if (shown !== indent) {
        tr ??= newState.tr;
        tr.setNodeAttribute(pos, "indent", shown);
      }
      prevIndent = shown;
      pos += node.nodeSize;
    }
    return tr;
  },
});

function idsIn(edit: Edit): string[] {
  const out: string[] = [];
  if ("block" in edit) out.push(edit.block);
  if ("after" in edit && edit.after !== null) out.push(edit.after);
  return out;
}

function withIds(edit: Edit, resolve: (id: string) => string): Edit {
  const e = { ...edit } as Edit & { block?: string; after?: string | null };
  if (e.block !== undefined) e.block = resolve(e.block);
  if (e.after !== undefined && e.after !== null) e.after = resolve(e.after);
  return e;
}

/** Finds a block's position by ID. Linear, so only for the rare cases that need it. */
export function findBlock(doc: Node, id: string): number | null {
  let pos = 0;
  for (let i = 0; i < doc.childCount; i++) {
    const node = doc.child(i);
    if (attrsOf(node).id === id) return pos;
    pos += node.nodeSize;
  }
  return null;
}

/** The binding for one open page. */
export class PageSync {
  private queue: LocalEdit[] = [];
  private sending: Promise<void> | null = null;
  private readonly alias = new Map<string, string>();
  private view: EditorView | null = null;
  /** The number of edits sent to the core, for tests and diagnostics. */
  sent = 0;

  constructor(
    private readonly client: CoreClient,
    readonly page: string,
    private readonly onError: (error: unknown) => void = (e) => console.error(e),
  ) {}

  attach(view: EditorView): void {
    this.view = view;
  }

  /** ProseMirror's `dispatchTransaction`: apply, then queue what changed for the core. */
  dispatch(tr: Transaction): void {
    const view = this.view;
    if (!view) return;
    const before = view.state;
    const { state, transactions } = before.applyTransaction(tr);
    view.updateState(state);
    if (!transactions.some((t) => t.docChanged) || transactions.every(isFromCore)) return;
    this.record(before, state, transactions);
  }

  private record(before: EditorState, after: EditorState, transactions: readonly Transaction[]): void {
    const steps = transactions.filter((t) => !isFromCore(t)).flatMap((t) => t.steps);
    const region = changedRegion(before.doc, after.doc, steps);
    if (!region) return;
    let edits: LocalEdit[];
    try {
      edits = diffBlocks(
        blocksIn(before.doc, region.start, region.oldEnd),
        blocksIn(after.doc, region.start, region.newEnd),
      );
    } catch (error) {
      this.fail(error);
      return;
    }
    if (edits.length === 0) return;
    this.queue.push(...edits);
    void this.pump();
  }

  /** Resolves once everything queued has reached the core. */
  async flush(): Promise<void> {
    while (this.sending) await this.sending;
  }

  private pump(): Promise<void> {
    this.sending ??= this.send().finally(() => {
      this.sending = null;
    });
    return this.sending;
  }

  private async send(): Promise<void> {
    while (this.queue.length > 0) {
      // A batch ends before the first edit that needs a block this batch creates.
      const creating = new Set<string>();
      let n = 0;
      for (const le of this.queue) {
        if (idsIn(le.edit).some((id) => creating.has(id))) break;
        if (le.creates) creating.add(le.creates);
        n += 1;
      }
      const batch = this.queue.splice(0, n);
      const resolve = (id: string) => this.alias.get(id) ?? id;
      let outcome;
      try {
        outcome = await this.client.edit(this.page as never, batch.map((le) => withIds(le.edit, resolve)));
      } catch (error) {
        this.queue = [];
        this.fail(error);
        return;
      }
      this.sent += batch.length;
      const creators = batch.filter((le) => le.creates !== undefined);
      creators.forEach((le, i) => {
        const real = outcome.created[i];
        if (real !== undefined) this.alias.set(le.creates!, real);
      });
      this.adopt(creators.map((le) => le.creates!), outcome.patch);
    }
  }

  /** Puts the core's IDs on blocks that had temporary ones, and the core's indents where they differ. */
  private adopt(created: string[], patch: PagePatch): void {
    const view = this.view;
    if (!view) return;
    const tr = view.state.tr;
    for (const tmp of created) {
      const real = this.alias.get(tmp);
      const pos = findBlock(tr.doc, tmp);
      if (real !== undefined && pos !== null) tr.setNodeAttribute(pos, "id", real);
    }
    // Indents the core changed by itself (a block's parent outdented or deleted).
    if (this.queue.length === 0) {
      for (const placed of patch.upserted) {
        const pos = findBlock(tr.doc, placed.block.id);
        if (pos === null) continue;
        if (attrsOf(tr.doc.nodeAt(pos)!).indent !== placed.block.indent) {
          tr.setNodeAttribute(pos, "indent", placed.block.indent);
        }
      }
    }
    if (tr.docChanged) {
      tr.setMeta(FROM_CORE, true).setMeta("addToHistory", false);
      view.dispatch(tr);
    }
  }

  private fail(error: unknown): void {
    this.onError(error);
    void this.reload();
  }

  /** Replaces what the editor shows with what the core holds. */
  async reload(view?: PageView): Promise<void> {
    await this.flush();
    this.queue = [];
    this.alias.clear();
    let fresh = view;
    if (!fresh) {
      // Opening is counted, so this read is matched by a close; the editor's own open remains.
      fresh = await this.client.openPage(this.page as never);
      await this.client.closePage(this.page as never);
    }
    this.replace(fresh.blocks);
  }

  replace(blocks: Block[]): void {
    const view = this.view;
    if (!view) return;
    const doc = docNode(blocks);
    const tr = view.state.tr.replaceWith(0, view.state.doc.content.size, doc.content);
    tr.setMeta(FROM_CORE, true).setMeta("addToHistory", false);
    view.dispatch(tr);
  }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

// An in-memory stand-in for the core, with the same commands and the same edit semantics, for
// tests and for working on the UI in a plain browser (`npm run dev`). Nothing is saved.

import type { Transport } from "./client";
import type {
  ApiError,
  Block,
  BlockId,
  BlockKind,
  Command,
  Edit,
  Event,
  Marks,
  PageId,
  PageInfo,
  PagePatch,
  Response,
  Span,
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
}

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

export class MemoryCore {
  private pages: MemPage[] = [];
  /** How many views have each page open, as the core counts them. */
  private readonly open = new Map<PageId, number>();
  private readonly listeners = new Set<(e: Event) => void>();

  readonly transport: Transport = {
    call: async (command) => this.handle(command),
    subscribe: (handler) => {
      this.listeners.add(handler);
      return () => this.listeners.delete(handler);
    },
  };

  private page(id: PageId): MemPage {
    return this.pages.find((p) => p.id === id) ?? fail("unknownPage", `no page ${id}`);
  }

  private info(): PageInfo[] {
    const out: PageInfo[] = [];
    const walk = (p: MemPage) => {
      out.push({ id: p.id, parent: p.parent, title: p.title, children: [...p.children] });
      for (const c of p.children) walk(this.page(c));
    };
    for (const p of this.pages.filter((p) => p.parent === null)) walk(p);
    return out;
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
        const gone = new Set<PageId>();
        const walk = (id: PageId) => {
          gone.add(id);
          for (const c of this.page(id).children) walk(c);
        };
        walk(command.page);
        const p = this.page(command.page);
        if (p.parent !== null) this.page(p.parent).children = this.page(p.parent).children.filter((c) => c !== p.id);
        this.pages = this.pages.filter((x) => !gone.has(x.id));
        this.open.delete(command.page);
        this.emit({ type: "pagesChanged", pages: this.info() });
        return { type: "done" };
      }
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
      case "ready":
        return { type: "done" };
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

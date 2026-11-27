// SPDX-License-Identifier: AGPL-3.0-or-later

// Attachment blocks: a file on the page, shown inline when it's an image and as a card
// otherwise, with the block's text as its caption. Files come in by pasting, dropping, or
// `/file`.

import type { Node } from "prosemirror-model";
import { TextSelection } from "prosemirror-state";
import type { EditorView, NodeView } from "prosemirror-view";

import { CoreError, type CoreClient } from "../core/client";
import type { BlockKind } from "../core/types";
import { attrsOf } from "./convert";
import { blockType } from "./schema";

type Attachment = Extract<BlockKind, { type: "attachment" }>;

let client: CoreClient | null = null;
/** Object URLs of files already read, by attachment ID. */
const urls = new Map<string, Promise<string>>();

export function setAttachmentClient(c: CoreClient | null): void {
  client = c;
  for (const url of urls.values()) void url.then((u) => URL.revokeObjectURL(u), () => {});
  urls.clear();
}

function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

function fromBase64(data: string): Uint8Array<ArrayBuffer> {
  const s = atob(data);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/** A file's bytes as an object URL, waiting for it if it's still coming from the server. */
export function attachmentUrl(blob: string, mime: string): Promise<string> {
  let url = urls.get(blob);
  if (!url) {
    url = load(blob, mime);
    urls.set(blob, url);
    url.catch(() => urls.delete(blob));
  }
  return url;
}

async function load(blob: string, mime: string): Promise<string> {
  const c = client;
  if (!c) throw new Error("no core for attachments");
  for (;;) {
    try {
      const data = await c.readAttachment(blob);
      return URL.createObjectURL(new Blob([fromBase64(data)], { type: mime || "application/octet-stream" }));
    } catch (e) {
      if (!(e instanceof CoreError) || e.code !== "notHere") throw e;
      await new Promise<void>((resolve) => {
        const stop = c.subscribe((event) => {
          if (event.type === "attachmentArrived" && event.blob === blob) {
            stop();
            resolve();
          }
        });
      });
    }
  }
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** Keeps files on the device and puts an attachment block for each after `pos`'s block. */
export async function insertFiles(view: EditorView, files: File[], pos?: number): Promise<void> {
  const c = client;
  if (!c || files.length === 0) return;
  const kinds: BlockKind[] = [];
  for (const file of files) {
    const data = toBase64(new Uint8Array(await file.arrayBuffer()));
    kinds.push(await c.addAttachment(file.name, file.type, data));
  }
  const state = view.state;
  const $at = state.doc.resolve(Math.min(pos ?? state.selection.from, state.doc.content.size));
  const block = $at.depth >= 1 ? $at.node(1) : state.doc.firstChild!;
  const after = $at.depth >= 1 ? $at.after(1) : state.doc.content.size;
  const indent = attrsOf(block).indent;
  const empty = block.content.size === 0 && attrsOf(block).kind.type === "paragraph";
  let tr = state.tr;
  // An empty paragraph becomes the first attachment rather than staying above it.
  let at = after;
  let first = 0;
  if (empty) {
    tr = tr.setNodeAttribute($at.before(1), "kind", kinds[0]!);
    first = 1;
  }
  for (const kind of kinds.slice(first)) {
    const node = blockType.create({ id: null, kind, indent });
    tr = tr.insert(at, node);
    at += node.nodeSize;
  }
  const para = blockType.create({ id: null, kind: { type: "paragraph" }, indent });
  tr = tr.insert(at, para);
  tr = tr.setSelection(TextSelection.create(tr.doc, at + 1));
  view.dispatch(tr.scrollIntoView());
}

/** Transaction meta asking the editor to open the file picker (`/file`). */
export const PICK_FILES = "drusite.pickFiles";

/** Opens the file picker and inserts what's chosen. */
export function pickFiles(view: EditorView): void {
  const input = document.createElement("input");
  input.type = "file";
  input.multiple = true;
  input.className = "file-picker";
  input.addEventListener("change", () => {
    void insertFiles(view, [...(input.files ?? [])]);
    input.remove();
  });
  document.body.append(input);
  input.click();
}

function control(tag: string, className: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = className;
  e.contentEditable = "false";
  return e;
}

export function attachmentView(node: Node, setAttrs: (n: Node) => void, dom: HTMLElement, contentDOM: HTMLElement): NodeView {
  const shown = control("div", "attachment-file");
  contentDOM.classList.add("attachment-caption");
  contentDOM.dataset.placeholder = "Add a caption";
  let current = "";

  const render = (kind: Attachment) => {
    const key = `${kind.blob}/${kind.mime}/${kind.name}`;
    if (key === current) return;
    current = key;
    shown.replaceChildren();
    if (kind.mime.startsWith("image/")) {
      const img = document.createElement("img");
      img.alt = kind.name;
      img.className = "attachment-image";
      shown.append(img);
      attachmentUrl(kind.blob, kind.mime).then(
        (url) => (img.src = url),
        () => shown.classList.add("attachment-missing"),
      );
    } else {
      const link = document.createElement("a");
      link.className = "attachment-card";
      link.download = kind.name;
      link.textContent = `📎 ${kind.name}`;
      const size = document.createElement("span");
      size.className = "attachment-size";
      size.textContent = formatSize(kind.size);
      link.append(size);
      shown.append(link);
      attachmentUrl(kind.blob, kind.mime).then(
        (url) => (link.href = url),
        () => shown.classList.add("attachment-missing"),
      );
    }
  };
  const kind = attrsOf(node).kind;
  if (kind.type === "attachment") render(kind);
  dom.append(shown, contentDOM);
  return {
    dom,
    contentDOM,
    update(next) {
      const k = attrsOf(next).kind;
      if (k.type !== "attachment") return false;
      node = next;
      render(k);
      setAttrs(next);
      return true;
    },
    stopEvent: (e) => shown.contains(e.target as globalThis.Node),
    ignoreMutation: (m) => shown.contains(m.target as globalThis.Node),
  };
}

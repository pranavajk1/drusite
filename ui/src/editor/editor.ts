// SPDX-License-Identifier: AGPL-3.0-or-later

// Putting the editor together for one open page.

import { history } from "prosemirror-history";
import { EditorState } from "prosemirror-state";
import { EditorView } from "prosemirror-view";

import type { CoreClient } from "../core/client";
import type { PageView } from "../core/types";
import { PICK_FILES, insertFiles, pickFiles } from "./attachments";
import { blockView, displayPlugin } from "./blocks";
import { baseKeys, editorKeymap, markdownShortcuts } from "./commands";
import { docNode } from "./convert";
import { clipboardParser } from "./schema";
import { PageSync, idPlugin } from "./sync";

export interface Editor {
  view: EditorView;
  sync: PageSync;
  /** Sends anything still queued to the core, then tears the editor down. */
  close(): Promise<void>;
}

export function createEditor(
  mount: HTMLElement,
  client: CoreClient,
  page: PageView,
  onError?: (error: unknown) => void,
): Editor {
  const sync = new PageSync(client, page.id, onError);
  const state = EditorState.create({
    doc: docNode(page.blocks),
    plugins: [markdownShortcuts(), editorKeymap(), baseKeys(), history(), idPlugin, displayPlugin],
  });
  const view = new EditorView(mount, {
    state,
    nodeViews: { block: blockView },
    clipboardParser,
    dispatchTransaction: (tr) => {
      sync.dispatch(tr);
      if (tr.getMeta(PICK_FILES)) pickFiles(view);
    },
    // Files pasted or dropped become attachments.
    handlePaste: (v, event) => {
      const files = [...(event.clipboardData?.files ?? [])];
      if (files.length === 0) return false;
      insertFiles(v, files).catch((e: unknown) => onError?.(e));
      return true;
    },
    handleDrop: (v, event) => {
      const files = [...(event.dataTransfer?.files ?? [])];
      if (files.length === 0) return false;
      const pos = v.posAtCoords({ left: event.clientX, top: event.clientY })?.pos;
      insertFiles(v, files, pos).catch((e: unknown) => onError?.(e));
      return true;
    },
    attributes: { class: "editor", spellcheck: "true", "aria-label": "Page content" },
  });
  sync.attach(view);
  return {
    view,
    sync,
    async close() {
      await sync.flush();
      view.destroy();
    },
  };
}

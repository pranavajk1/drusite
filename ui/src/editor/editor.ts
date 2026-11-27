// SPDX-License-Identifier: AGPL-3.0-or-later

// Putting the editor together for one open page.

import { history } from "prosemirror-history";
import { EditorState } from "prosemirror-state";
import { EditorView } from "prosemirror-view";

import type { CoreClient } from "../core/client";
import type { PageId, PageView } from "../core/types";
import { PICK_FILES, insertFiles, pickFiles } from "./attachments";
import { blockView, displayPlugin } from "./blocks";
import { baseKeys, editorKeymap, markdownShortcuts } from "./commands";
import { docNode } from "./convert";
import { clipboardParser } from "./schema";
import { slashMenu } from "./slash";
import { PageSync, idPlugin } from "./sync";

/** How a link to another page is written: the page's ID after this. */
export const PAGE_LINK = "drusite:page/";

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
  openPage?: (id: PageId) => void,
): Editor {
  const sync = new PageSync(client, page.id, onError);
  const state = EditorState.create({
    doc: docNode(page.blocks),
    plugins: [slashMenu(), markdownShortcuts(), editorKeymap(), baseKeys(), history(), idPlugin, displayPlugin],
  });
  const view = new EditorView(mount, {
    state,
    nodeViews: { block: blockView },
    clipboardParser,
    dispatchTransaction: (tr) => {
      sync.dispatch(tr);
      if (tr.getMeta(PICK_FILES)) pickFiles(view);
    },
    // ⌘- or Ctrl-click on a link to another page opens it.
    handleClick: (v, pos, event) => {
      if (!(event.metaKey || event.ctrlKey) || !openPage) return false;
      const href = v.state.doc
        .resolve(pos)
        .marks()
        .find((m) => m.type.name === "link")
        ?.attrs.href as string | undefined;
      const id = href?.startsWith(PAGE_LINK) ? href.slice(PAGE_LINK.length) : null;
      if (!id) return false;
      openPage(id as PageId);
      return true;
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

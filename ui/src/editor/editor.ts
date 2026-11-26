// SPDX-License-Identifier: AGPL-3.0-or-later

// Putting the editor together for one open page.

import { history } from "prosemirror-history";
import { EditorState } from "prosemirror-state";
import { EditorView } from "prosemirror-view";

import type { CoreClient } from "../core/client";
import type { PageView } from "../core/types";
import { blockView, displayPlugin } from "./blocks";
import { baseKeys, editorKeymap, markdownShortcuts } from "./commands";
import { docNode } from "./convert";
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
    dispatchTransaction: (tr) => sync.dispatch(tr),
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

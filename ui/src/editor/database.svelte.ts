// SPDX-License-Identifier: AGPL-3.0-or-later

// An inline database in a page: a block whose kind names a database (a subpage of the page),
// shown with its views. The views are the Svelte `Database` component, mounted in the block.

import type { Node } from "prosemirror-model";
import type { EditorView, NodeView } from "prosemirror-view";
import { mount, unmount } from "svelte";

import type { PageId, PageInfo, Template } from "../core/types";
import Database from "../lib/database/Database.svelte";
import { attrsOf } from "./convert";
import { blockDom } from "./schema";
import { queryHost } from "./query";

/** Set on a transaction to make a database in the block the cursor is in. */
export const MAKE_DATABASE = "drusite.makeDatabase";

export function databaseView(node: Node, view: EditorView, getPos: () => number | undefined): NodeView {
  const dom = document.createElement("div");
  const setAttrs = (n: Node) => {
    const spec = blockDom(n) as [string, Record<string, string>];
    for (const [k, v] of Object.entries(spec[1])) dom.setAttribute(k, v);
  };
  const contentDOM = document.createElement("div");
  contentDOM.hidden = true;
  const widget = document.createElement("div");
  widget.className = "block-database";
  widget.contentEditable = "false";
  dom.append(widget, contentDOM);
  setAttrs(node);

  const host = queryHost();
  const kind = attrsOf(node).kind;
  if (kind.type !== "database" || !host) return { dom, contentDOM };
  let current = kind.database;
  const props = $state({ pages: host.pages() as PageInfo[] });
  const stop = host.client.subscribe((e) => {
    if (e.type === "pagesChanged") props.pages = e.pages;
  });
  let app: ReturnType<typeof mount> | null = null;
  const show = (database: PageId, viewId: string | null) => {
    if (app) void unmount(app);
    app = mount(Database, {
      target: widget,
      props: {
        client: host.client,
        database,
        view: viewId,
        get pages() {
          return props.pages;
        },
        inline: true,
        onopen: host.open,
        onerror: (e: unknown) => console.error(e),
        onview: (id: string) => {
          // The view shown is kept in the block, so every device opens the same one.
          const pos = getPos();
          if (pos === undefined) return;
          const k = attrsOf(view.state.doc.nodeAt(pos)!).kind;
          if (k.type === "database" && k.view !== id) view.dispatch(view.state.tr.setNodeAttribute(pos, "kind", { ...k, view: id }));
        },
      },
    });
  };
  show(kind.database, kind.view);

  return {
    dom,
    contentDOM,
    update(next) {
      const k = attrsOf(next).kind;
      if (k.type !== "database") return false;
      setAttrs(next);
      if (k.database !== current) {
        current = k.database;
        show(k.database, k.view);
      }
      return true;
    },
    stopEvent: (e) => widget.contains(e.target as globalThis.Node),
    ignoreMutation: (m) => widget.contains(m.target as globalThis.Node),
    destroy: () => {
      stop();
      if (app) void unmount(app);
    },
  };
}

/** Makes a database (from `template`) inside the page, and puts it in the block at `pos`. */
export async function makeDatabase(view: EditorView, page: PageId, template: Template): Promise<void> {
  const host = queryHost();
  if (!host) return;
  const title = template === "tasks" ? "Tasks" : "Database";
  const { page: db } = await host.client.createDatabase(page, title, true, template);
  const { state } = view;
  const at = state.selection.$from.before(1);
  const node = state.doc.nodeAt(at);
  if (!node) return;
  view.dispatch(state.tr.setNodeAttribute(at, "kind", { type: "database", database: db.id, view: null }));
}

// SPDX-License-Identifier: AGPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { blocks, citedIn, inlines } from "./render";

describe("answers", () => {
  it("turn citations into chips and keep everything else as text", () => {
    expect(inlines("The key is **under** the pot [2]. <img src=x onerror=alert(1)> [link](http://e.vil)")).toEqual([
      { kind: "text", text: "The key is " },
      { kind: "text", text: "under", bold: true },
      { kind: "text", text: " the pot " },
      { kind: "cite", n: 2 },
      { kind: "text", text: ". <img src=x onerror=alert(1)> [link](http://e.vil)" },
    ]);
  });

  it("make lists, headings and paragraphs", () => {
    const b = blocks("# Plan\nFirst:\n\n- eggs [1]\n- milk\n\n1. call\n2. pay");
    expect(b.map((x) => x.kind)).toEqual(["h", "p", "ul", "ol"]);
    expect(b[2]).toMatchObject({ kind: "ul", items: [[{ text: "eggs " }, { kind: "cite", n: 1 }], [{ text: "milk" }]] });
  });

  it("list the citations that name sources, in order", () => {
    expect(citedIn("a [3] b [1][3] c [9]", new Set([1, 3]))).toEqual([3, 1]);
  });
});

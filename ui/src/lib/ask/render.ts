// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * An answer, as the Ask panel shows it: a small, safe subset of Markdown (paragraphs, lists,
 * headings, bold, italic, code) and citations `[n]` as chips. It's never HTML: an answer can
 * quote a page, and a page can say anything, so nothing in it can load an image, follow a link
 * or run. Links stay text.
 */

export type Inline =
  | { kind: "text"; text: string; bold?: boolean; italic?: boolean }
  | { kind: "code"; text: string }
  | { kind: "cite"; n: number };

export type Block =
  | { kind: "p"; inlines: Inline[] }
  | { kind: "h"; inlines: Inline[] }
  | { kind: "ul"; items: Inline[][] }
  | { kind: "ol"; start: number; items: Inline[][] };

const TOKEN = /(\[\d{1,3}\])|(\*\*[^*\n]+\*\*)|(`[^`\n]+`)|(\*[^*\s\n][^*\n]*\*)|(_[^_\s\n][^_\n]*_)/g;

export function inlines(text: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  for (const m of text.matchAll(TOKEN)) {
    const at = m.index ?? 0;
    if (at > last) out.push({ kind: "text", text: text.slice(last, at) });
    const t = m[0];
    if (m[1]) out.push({ kind: "cite", n: Number(t.slice(1, -1)) });
    else if (m[2]) out.push({ kind: "text", text: t.slice(2, -2), bold: true });
    else if (m[3]) out.push({ kind: "code", text: t.slice(1, -1) });
    else out.push({ kind: "text", text: t.slice(1, -1), italic: true });
    last = at + t.length;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last) });
  return out;
}

export function blocks(text: string): Block[] {
  const out: Block[] = [];
  let para: string[] = [];
  const flush = () => {
    if (para.length) out.push({ kind: "p", inlines: inlines(para.join("\n")) });
    para = [];
  };
  for (const raw of text.replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.trimEnd();
    const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
    const numbered = /^\s*(\d{1,3})[.)]\s+(.*)$/.exec(line);
    const heading = /^\s*#{1,6}\s+(.*)$/.exec(line);
    const prev = out[out.length - 1];
    if (line.trim() === "") {
      flush();
    } else if (bullet) {
      flush();
      if (prev?.kind === "ul" && para.length === 0) prev.items.push(inlines(bullet[1]!));
      else out.push({ kind: "ul", items: [inlines(bullet[1]!)] });
    } else if (numbered) {
      flush();
      if (prev?.kind === "ol") prev.items.push(inlines(numbered[2]!));
      else out.push({ kind: "ol", start: Number(numbered[1]), items: [inlines(numbered[2]!)] });
    } else if (heading) {
      flush();
      out.push({ kind: "h", inlines: inlines(heading[1]!) });
    } else {
      para.push(line);
    }
  }
  flush();
  return out;
}

/** The citation numbers an answer uses, in order of first use, among `known`. */
export function citedIn(text: string, known: Set<number>): number[] {
  const seen: number[] = [];
  for (const m of text.matchAll(/\[(\d{1,3})\]/g)) {
    const n = Number(m[1]);
    if (known.has(n) && !seen.includes(n)) seen.push(n);
  }
  return seen;
}

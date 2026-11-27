// SPDX-License-Identifier: AGPL-3.0-or-later

// Attachments: `/file` picks files, an image shows inline and a document as a card, and the
// block's text is the caption.

import { expect, test } from "@playwright/test";

import { openApp } from "./helpers";

// A 1×1 PNG.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
  "base64",
);

test("/file attaches an image and a document, each with a caption", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("Trip photos");
  await page.keyboard.press("Enter");
  const chooser = page.waitForEvent("filechooser");
  await page.keyboard.type("/file ");
  await (await chooser).setFiles([
    { name: "beach.png", mimeType: "image/png", buffer: PNG },
    { name: "tickets.pdf", mimeType: "application/pdf", buffer: Buffer.from("%PDF-1.7 tickets") },
  ]);

  const image = page.locator(".editor .attachment-image");
  await expect(image).toHaveAttribute("alt", "beach.png");
  await expect(image).toHaveAttribute("src", /^blob:/);
  await expect.poll(() => image.evaluate((i: HTMLImageElement) => i.naturalWidth)).toBe(1);
  const card = page.locator(".editor .attachment-card");
  await expect(card).toContainText("tickets.pdf");
  await expect(card).toContainText("16 B");
  await expect(card).toHaveAttribute("download", "tickets.pdf");

  // The cursor waits on a new line below; the caption is the block's own text.
  await page.keyboard.type("After the files");
  await expect(page.locator(".editor .block").last()).toHaveText("After the files");
  await page.locator(".editor .block-attachment .attachment-caption").first().click();
  await page.keyboard.type("Low tide");
  await expect(page.locator(".editor .block-attachment").first()).toContainText("Low tide");
});

test("pasting an image attaches it", async ({ page }) => {
  await openApp(page);
  await page.evaluate((b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    const data = new DataTransfer();
    data.items.add(new File([bytes], "pasted.png", { type: "image/png" }));
    const editor = document.querySelector(".editor")!;
    editor.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, PNG.toString("base64"));
  await expect(page.locator(".editor .attachment-image")).toHaveAttribute("alt", "pasted.png");
});

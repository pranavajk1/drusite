// SPDX-License-Identifier: AGPL-3.0-or-later

// AI in Settings, on the in-memory core: a provider is added (its key never shown again), tested,
// picked per feature, changed and removed. Nothing here reaches a real provider.

import { expect, test } from "@playwright/test";

import { openApp } from "./helpers";

test("an AI provider is added, tested, picked per feature and removed", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "⚙️ Settings" }).click();
  const sheet = page.getByRole("dialog", { name: "Settings" });
  await expect(sheet.getByRole("heading", { name: "AI", exact: true })).toBeVisible();

  // A custom endpoint needs its address.
  await sheet.getByRole("button", { name: "Add a provider…" }).click();
  const form = sheet.getByRole("form", { name: "Add a provider" });
  await expect(form.getByLabel("Provider")).toHaveValue("openAiCompatible");
  await form.getByLabel("Name").fill("Home box");
  await form.getByLabel("Address", { exact: true }).fill("http://ai.local:4000/v1");
  await form.getByLabel("API key (if it needs one)").fill("sk-home-1234");
  await form.getByRole("button", { name: "Save" }).click();
  await expect(form).toHaveCount(0);

  const list = sheet.getByRole("list", { name: "AI providers" });
  await expect(list).toContainText("Home box");
  await expect(list).toContainText("OpenAI-compatible · http://ai.local:4000/v1 · key …1234");
  await expect(list).not.toContainText("sk-home");
  // Saving runs Test connection; the preview can't reach anything and says so.
  await expect(sheet.getByRole("list", { name: "Test connection: Home box" })).toContainText(
    "This preview can't reach providers",
  );

  // A vendor needs a key.
  await sheet.getByRole("button", { name: "Add a provider…" }).click();
  await form.getByLabel("Provider").selectOption("anthropic");
  await expect(form.getByLabel("Address (optional)")).toHaveAttribute("placeholder", "https://api.anthropic.com/v1");
  await form.getByLabel("API key", { exact: true }).fill("sk-ant-9876");
  await form.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("Anthropic");

  // Each feature picks its own provider; Anthropic isn't offered for embeddings.
  const chatProvider = sheet.getByLabel("Chat: provider");
  await chatProvider.selectOption({ label: "Anthropic" });
  await sheet.getByLabel("Chat: model").fill("claude-x");
  await sheet.getByLabel("Chat: model").press("Enter");
  const embedProvider = sheet.getByLabel("Embeddings: provider");
  await expect(embedProvider.locator("option")).toHaveText(["Off", "Home box"]);
  await embedProvider.selectOption({ label: "Home box" });
  await sheet.getByLabel("Embeddings: model").fill("nomic-embed-text");
  await sheet.getByLabel("Embeddings: model").blur();

  // Changing a provider keeps its key unless a new one is typed.
  await list.locator("li", { hasText: "Home box" }).getByRole("button", { name: "Edit" }).click();
  const change = sheet.getByRole("form", { name: "Change provider" });
  await expect(change.getByLabel("API key (if it needs one)")).toHaveAttribute("placeholder", "Saved (…1234); type to replace");
  await change.getByLabel("Name").fill("Home box (LiteLLM)");
  await change.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("Home box (LiteLLM)");
  await expect(list).toContainText("key …1234");

  // Closed and opened again: the choices were saved.
  await sheet.getByRole("button", { name: "Close" }).click();
  await page.getByRole("button", { name: "⚙️ Settings" }).click();
  await expect(sheet.getByLabel("Chat: model")).toHaveValue("claude-x");
  await expect(sheet.getByLabel("Embeddings: model")).toHaveValue("nomic-embed-text");

  // Removing a provider turns off the features that used it.
  await list.locator("li", { hasText: "Home box" }).getByRole("button", { name: "Remove" }).click();
  await expect(list).not.toContainText("Home box");
  await expect(sheet.getByLabel("Embeddings: provider")).toHaveValue("");
  await expect(sheet.getByLabel("Chat: provider")).not.toHaveValue("");
});

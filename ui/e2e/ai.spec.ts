// SPDX-License-Identifier: AGPL-3.0-or-later

// AI in Settings, on the in-memory core: a provider is added (its key never shown again), tested,
// picked per feature, changed and removed. Nothing here reaches a real provider.

import { expect, test } from "@playwright/test";

import { choose, openApp, openSettings } from "./helpers";

test("an AI provider is added, tested, picked per feature and removed", async ({
  page,
}) => {
  await openApp(page);
  const sheet = await openSettings(page, "AI");
  await expect(
    sheet.getByRole("heading", { name: "AI", exact: true }),
  ).toBeVisible();

  // A custom endpoint needs its address.
  await sheet.getByRole("button", { name: "Add a provider…" }).click();
  const form = sheet.getByRole("form", { name: "Add a provider" });
  await expect(form.getByLabel("Provider")).toHaveText("OpenAI-compatible");
  await form.getByLabel("Name").fill("Home box");
  await form
    .getByLabel("Address", { exact: true })
    .fill("http://ai.local:4000/v1");
  await form.getByLabel("API key (if it needs one)").fill("sk-home-1234");
  await form.getByRole("button", { name: "Save" }).click();
  await expect(form).toHaveCount(0);

  const list = sheet.getByRole("list", { name: "AI providers" });
  await expect(list).toContainText("Home box");
  await expect(list).toContainText(
    "OpenAI-compatible · http://ai.local:4000/v1 · key …1234",
  );
  await expect(list).not.toContainText("sk-home");
  // Saving runs Test connection; the preview can't reach anything and says so.
  await expect(
    sheet.getByRole("list", { name: "Test connection: Home box" }),
  ).toContainText("This preview can't reach providers");

  // A vendor needs a key.
  await sheet.getByRole("button", { name: "Add a provider…" }).click();
  await choose(page, form.getByLabel("Provider"), "Anthropic");
  await expect(form.getByLabel("Address (optional)")).toHaveAttribute(
    "placeholder",
    "https://api.anthropic.com/v1",
  );
  await form.getByLabel("API key", { exact: true }).fill("sk-ant-9876");
  await form.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("Anthropic");

  // Each feature picks its own provider; Anthropic isn't offered for embeddings.
  const chatProvider = sheet.getByLabel("Chat: provider");
  await choose(page, chatProvider, "Anthropic");
  await sheet.getByLabel("Chat: model").fill("claude-x");
  await sheet.getByLabel("Chat: model").press("Enter");
  const embedProvider = sheet.getByLabel("Embeddings: provider");
  await embedProvider.click();
  await expect(page.getByRole("listbox").getByRole("option")).toHaveText([
    "Off",
    "Home box",
  ]);
  await page
    .getByRole("listbox")
    .getByRole("option", { name: "Home box" })
    .click();
  await sheet.getByLabel("Embeddings: model").fill("nomic-embed-text");
  await sheet.getByLabel("Embeddings: model").blur();

  // Changing a provider keeps its key unless a new one is typed.
  await list
    .locator("li", { hasText: "Home box" })
    .getByRole("button", { name: "Edit" })
    .click();
  const change = sheet.getByRole("form", { name: "Change provider" });
  await expect(change.getByLabel("API key (if it needs one)")).toHaveAttribute(
    "placeholder",
    "Saved (…1234); type to replace",
  );
  await change.getByLabel("Name").fill("Home box (LiteLLM)");
  await change.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("Home box (LiteLLM)");
  await expect(list).toContainText("key …1234");

  // Closed and opened again: the choices were saved.
  await sheet.getByRole("button", { name: "Close" }).click();
  await openSettings(page, "AI");
  await expect(sheet.getByLabel("Chat: model")).toHaveValue("claude-x");
  await expect(sheet.getByLabel("Embeddings: model")).toHaveValue(
    "nomic-embed-text",
  );

  // Removing a provider turns off the features that used it.
  await list
    .locator("li", { hasText: "Home box" })
    .getByRole("button", { name: "Remove" })
    .click();
  await expect(list).not.toContainText("Home box");
  await expect(sheet.getByLabel("Embeddings: provider")).toHaveText("Off");
  await expect(sheet.getByLabel("Chat: provider")).not.toHaveText("Off");
});

test("AI settings show the server's models for Ask and search", async ({
  page,
}) => {
  await openApp(page);
  const sheet = await openSettings(page, "AI");
  const server = sheet.getByRole("group", { name: "On your server" });
  await expect(server).toContainText("[ask.chat]");
  // The in-memory core's server has a chat model and no embedding model.
  await expect(sheet.getByTestId("ai-server-chat")).toHaveText("Memory");
  await expect(sheet.getByTestId("ai-server-embeddings")).toHaveText(
    "Off: this server has no embedding model, so search matches words only.",
  );
  // The device's providers are separate, and say Ask doesn't use them.
  await expect(
    sheet.getByRole("heading", { name: "On this device" }),
  ).toBeVisible();
  await expect(sheet).toContainText("Ask your notes doesn't use these");
});

// SPDX-License-Identifier: AGPL-3.0-or-later

// Agents in Settings, on the in-memory core: connecting one on this computer shows the command
// once, it's listed with what it may do, and it can be disconnected.

import { expect, test } from "@playwright/test";

import { openApp, openSettings } from "./helpers";

test("an agent on this computer is connected, listed and disconnected", async ({ page }) => {
  await openApp(page);
  const sheet = await openSettings(page, "Agents");
  await expect(sheet.getByRole("heading", { name: "Agents", exact: true })).toBeVisible();
  await expect(sheet.getByText("Nothing yet.")).toBeVisible();

  await sheet.getByLabel("Let agents on this computer connect").check();
  await expect(sheet.getByText("http://127.0.0.1:47321/mcp", { exact: false })).toBeVisible();
  await sheet.getByLabel("Connect an agent").fill("Claude Code");
  await sheet.getByLabel("Read, add and edit pages").check();
  await sheet.getByLabel("Edit journal entries too").check();
  await sheet.getByRole("button", { name: "Connect", exact: true }).click();

  const made = sheet.getByTestId("agent-made");
  await expect(made).toContainText("claude mcp add --transport http drusite-claude-code http://127.0.0.1:47321/mcp");
  await expect(made).toContainText('--header "Authorization: Bearer drusite_local_');
  await made.getByRole("button", { name: "Done" }).click();
  await expect(made).toHaveCount(0);

  const list = sheet.getByRole("list", { name: "Agents on this computer" });
  await expect(list).toContainText("Claude Code");
  await expect(list).toContainText("reads and edits, journal too");
  await list.getByRole("button", { name: "Disconnect" }).click();
  await expect(list).toHaveCount(0);
});

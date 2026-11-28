// SPDX-License-Identifier: AGPL-3.0-or-later

// Settings → Account & sync names the app's version, and the server's once sync has connected
// to one that says (older servers don't, and then there's no row for it).

import { expect, test } from "@playwright/test";

import { openApp, openSettings } from "./helpers";

const version = /^\d+\.\d+\.\d+(-[\w.]+)?$/;

test("the app's version, and the server's when it gave one", async ({ page }) => {
  await openApp(page);
  const sheet = await openSettings(page, "Account & sync");
  const about = sheet.getByRole("group", { name: "About" });
  await expect(about.getByLabel("App version")).toHaveText(version);
  await expect(about.getByLabel("Server version")).toHaveCount(0);

  // Signed in to a server from before versions were shown: still no row.
  const signedIn = (serverVersion: string | null) =>
    page.evaluate(
      (v) =>
        window.drusiteMemoryCore!.setAccount({
          server: "https://notes.example.test/",
          email: "ada@example.test",
          phase: "online",
          message: null,
          serverVersion: v,
        }),
      serverVersion,
    );
  await signedIn(null);
  await expect(sheet.getByText("ada@example.test").first()).toBeVisible();
  await expect(about.getByLabel("Server version")).toHaveCount(0);

  await signedIn("0.1.0-alpha.7");
  await expect(about.getByLabel("Server version")).toHaveText("0.1.0-alpha.7");
});

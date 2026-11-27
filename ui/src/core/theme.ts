// SPDX-License-Identifier: AGPL-3.0-or-later

// Light or dark. The system decides unless the user picked one in Settings → Appearance. The
// choice is this device's, like Notion's: a phone and a laptop can differ.

export type ThemeChoice = "system" | "light" | "dark";

const KEY = "drusite.theme";

export function themeChoice(): ThemeChoice {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

export function setThemeChoice(choice: ThemeChoice) {
  if (choice === "system") localStorage.removeItem(KEY);
  else localStorage.setItem(KEY, choice);
  applyTheme();
}

/** Puts the choice on <html>; the stylesheet does the rest. Also tells the phone's status bar. */
export function applyTheme() {
  const choice = themeChoice();
  const root = document.documentElement;
  if (choice === "system") delete root.dataset.theme;
  else root.dataset.theme = choice;
  const dark = choice === "dark" || (choice === "system" && (window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false));
  let meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (!meta) {
    meta = document.createElement("meta");
    meta.name = "theme-color";
    document.head.append(meta);
  }
  meta.content = dark ? "#1b1a1f" : "#ffffff";
}

/** Follows the system while the choice is "system". */
export function watchSystemTheme(): () => void {
  const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
  if (!mq) return () => {};
  const on = () => applyTheme();
  mq.addEventListener("change", on);
  return () => mq.removeEventListener("change", on);
}

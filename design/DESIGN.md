# Drusite design standard

This is how Drusite looks and behaves, on every platform: macOS, Windows, Linux, iOS, Android and the
browser build. New screens follow it. When a change needs something this document doesn't cover, add it
here in the same change.

The code is the other half of the standard:

| What | Where |
|---|---|
| Tokens (every raw value) | `ui/src/styles/tokens.css` |
| Element defaults | `ui/src/styles/base.css` |
| Controls | `ui/src/styles/controls.css`, `ui/src/lib/ui/` |
| App frame | `ui/src/styles/shell.css` |
| Page, editor, journal | `ui/src/styles/page.css`, `editor.css`, `journal.css` |
| Modals, search, toasts, menus | `ui/src/styles/overlays.css` |
| Settings | `ui/src/styles/settings.css` |
| Icons | `ui/src/lib/icons/icons.ts` (Lucide, ISC) |
| Server pages (sign-in, consent) | `crates/drusite-server/assets/signin.css` |

## 1. Principles

Drusite takes **Notion's design language**: a quiet, near-monochrome tool that gets out of the way
of the writing.

1. **Content first.** The page is plain: black-on-white text in a centred column. Chrome is grey and
   small, and much of it appears only on hover.
2. **One accent, used sparingly.** Amethyst violet marks the primary action, the current selection,
   focus and links between pages. Nothing else is coloured, apart from mood and status.
3. **Hover reveals, touch shows.** On a desktop, row actions, chevrons and remove buttons appear on
   hover. On a touch screen, where there is no hover, they're always visible and at least 36 px.
4. **Custom controls, native behaviour.** Selects, switches, checkboxes and menus are drawn by the app,
   but they keep the keyboard behaviour of the native ones.
5. **One app, three shapes.** Desktop, tablet and phone use the same components. Phones follow the
   Notion iOS and Android apps (top bar, bottom tab bar, sheets, full-screen settings), rather than
   squeezing the desktop layout.

### What we take from Notion, and what we don't

We take the **language**: the proportions, the spacing rhythm, the hover-reveal pattern, the left-nav
settings modal, the slash menu, ⌘K search, the dark toast with Undo, and the bottom tab bar on phones.

We **don't take Notion's assets**. The repository is public, so there are no Notion fonts, icons,
illustrations, colours copied by value, or CSS. The type is the system font stack, the icons are
Lucide, and every token in `tokens.css` was chosen for Drusite. The neutrals carry a faint violet
tint where Notion's are warm.

### Where Drusite departs from Notion

| Notion | Drusite | Why |
|---|---|---|
| Blue accent (`#2383e2`-ish) | **Amethyst** `#6d45e0` (light), `#7a56ea` (dark) | From the app icon's crystal bed (`design/icon/`). A note app's accent should be its own. |
| Warm grey neutrals | Neutrals with a faint violet tint (`#f8f7fa` sidebar) | They sit next to the amethyst without clashing. |
| Inter-like custom font on some surfaces | **System font stack only** | Zero bundle cost, native rendering on every OS, and no font licence to track. |
| Page icons and covers | No covers. User emoji stay only where they're content: the callout's icon. | Covers are a feature Drusite doesn't have. Don't add UI for them. |
| Workspace switcher | One workspace, shown as the Drusite mark and name | Drusite has one workspace per account. |
| Grey mood-less journal | **Mood colours** (`--mood-1`…`--mood-5`) and Lucide faces | The journal is a Drusite feature. Mood is the only other colour scale. |
| Sync is invisible | **Sync status** is always on screen: a dot in the top bar and at the foot of the sidebar | Drusite is local-first and self-hosted, so whether a device is synced matters. |
| Settings mixes workspace and account | Settings groups: Account, Notes, Connections | Drusite's settings are mostly this device's and the account's. |

## 2. Colour

Every colour is a token. **Never write a hex value in a component or a stylesheet other than
`tokens.css`.** The exceptions are the toast (always dark) and the Markdown-select chevron (a data URL
can't read a variable).

| Token | Light | Dark | Use |
|---|---|---|---|
| `--bg` | `#ffffff` | `#1b1a1f` | Page background |
| `--bg-sidebar` | `#f8f7fa` | `#212026` | Sidebar, settings nav |
| `--bg-elevated` | `#ffffff` | `#26252c` | Popovers, modals, sheets |
| `--bg-subtle` | `#f4f3f6` | `#27262d` | Code, callouts, input fills |
| `--bg-hover` | violet-black at 5.5% | white at 6% | Hover on any row or button |
| `--bg-active` | violet-black at 9% | white at 10% | Pressed; the current row |
| `--text` | `#2b2930` | `#e8e6ed` | Body text, titles |
| `--text-2` | `#6f6c78` | `#a19eaa` | Secondary text: sidebar rows, descriptions, meta |
| `--text-3` | `#a29fab` | `#6f6c78` | Tertiary: placeholders, section labels, icons at rest |
| `--border` / `--border-strong` / `--divider` | | | Hairlines. Prefer `box-shadow: inset 0 0 0 1px` to `border`, so nothing shifts. |
| `--accent` | `#6d45e0` | `#7a56ea` | Primary buttons, switches, checked boxes, today in the calendar |
| `--accent-text` | `#6339d6` | `#b39cfa` | Accent as text: links to pages, the selected option's tick |
| `--accent-soft`, `--accent-soft-2` | | | Search highlights, the revealed-block flash, the avatar |
| `--focus` | accent at 45% | accent at 55% | The focus ring |
| `--danger`, `--danger-soft` | | | Delete forever, Sign out, errors |
| `--success`, `--warning` | | | The sync dot (online / connecting, offline) |
| `--mood-1` … `--mood-5` | red → green | muted | Mood only |

**Contrast** (WCAG 2.2, against `--bg` unless noted):

| Pair | Light | Dark |
|---|---|---|
| `--text` | 14.4:1 | 14.0:1 |
| `--text-2` | 5.1:1 (4.8:1 on `--bg-sidebar`) | 6.6:1 |
| `--accent-text` | 6.8:1 | 7.5:1 |
| White on `--accent` (primary buttons) | 5.9:1 | 4.8:1 |
| `--accent` against `--bg` (switches, checked boxes) | 5.9:1 | 3.6:1 |
| `--text-3` | 2.6:1 | 3.4:1 |

`--text-2` passes AA for body text. `--text-3` doesn't, so it's only for placeholders, decorative
icons and section labels that something else also conveys. Don't use it for sentences. The dark accent
is darker than the light-on-dark violet you might expect, so white button labels keep 4.5:1.

**Theme.** Light by default, dark when the system is dark. Settings → Appearance can override that per
device: "Use system setting", "Light" or "Dark", stored in `localStorage["drusite.theme"]` and applied as
`<html data-theme>` before the first paint (`ui/src/core/theme.ts`). `<meta name="theme-color">` follows
it, so the phone's status bar matches.

## 3. Type

The system stack: `ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI Variable Text", "Segoe
UI", system-ui, Roboto, …`, and `ui-monospace, "SF Mono", Menlo, Consolas, …` for code. That gives SF
Pro on Apple, Segoe UI Variable on Windows 11, Roboto on Android, and Cantarell or Noto on Linux. No web
fonts are shipped.

| Token | Size | Use |
|---|---|---|
| `--text-title` | 40 px, 700, line height 1.2, −0.01em | Page title (30 px on phones) |
| `--text-h1/h2/h3` | 1.875 / 1.5 / 1.25 em, 600 | Headings in the page |
| `--text-base` | 16 px, line height 1.6 | Page body |
| `--text-xl` | 22 px, 600 | Settings section titles |
| `--text-lg` | 18 px | Modal titles, the search input |
| `--text-md` | 14 px (16 on touch) | All UI: sidebar, menus, buttons, settings rows |
| `--text-sm` | 13 px (14 on touch) | Descriptions, meta lines |
| `--text-xs` | 12 px, 500 | Section labels ("Pages", "Basic blocks"), shortcuts |

Weights are 400, 500 (UI labels, sidebar rows), 600 (headings, titles in UI) and 700 (page title only).
On touch screens every input is at least 16 px, or iOS zooms in when it gets the focus.

## 4. Space, size and radius

A **4 px scale**: `--space-1` 4, `-2` 8, `-3` 12, `-4` 16, `-5` 20, `-6` 24, `-8` 32, `-10` 40, `-12`
48, `-16` 64, `-24` 96, plus `--space-0` 2 for hairline gaps.

| Token | Value | Use |
|---|---|---|
| `--row` | 28 px (40 on touch) | Sidebar rows, menu items, small buttons, inline selects |
| `--control` | 32 px (44 on touch) | Inputs and buttons |
| `--sidebar-width` | 248 px | Desktop sidebar |
| `--page-width` | 720 px | The text column |
| `--page-pad` | 96 px (16 on phones) | Side padding around the column |
| `--topbar` | 44 px | Top bar |
| `--tabbar` | 56 px + safe area | Phone tab bar |

**Radius:** `--radius-xs` 3 (inline code), `-sm` 6 (rows, buttons, inputs), `-md` 8 (cards, callouts,
code blocks), `-lg` 12 (popovers, modals), `-xl` 16 (phone sheets), `-full` (pills, dots, the switch).

**Icons:** 16 px in UI rows, 14 px in tight hover buttons, 18–20 px in the phone top bar and toolbar,
22 px in the tab bar, 28 px in empty states.

## 5. Elevation, motion, layers

| Token | Use |
|---|---|
| `--shadow-sm` | Phone settings groups, a property form: one step above the page |
| `--shadow-md` | Popovers and menus |
| `--shadow-lg` | Modals, sheets, the toast, the open phone drawer |

In dark mode shadows barely show, so each one carries a 1 px white-alpha ring.

**Motion** is short and only confirms what happened:

| Token | Value | Use |
|---|---|---|
| `--dur-fast` | 90 ms | Hover and press colour |
| `--dur` | 150 ms | Popovers in (scale 0.97 → 1, fade), the chevron rotating, the switch |
| `--dur-slow` | 240 ms | Modals, sheets from the bottom, the drawer, the toast |
| `--ease-out` | `cubic-bezier(0.2, 0.8, 0.2, 1)` | Anything entering |

`prefers-reduced-motion: reduce` sets all three durations to 0.

**Layers** (`--z-*`): sticky top bar 10, tab bar 20, drawer 30, phone toolbar 35, modal 40, popover 50,
toast 60. Popovers are portalled to `<body>` (`ui/src/lib/ui/floating.ts`), so no transformed or
scrolling parent clips them.

## 6. Icons

**Lucide** (ISC licence, compatible with AGPL-3.0-or-later; the notice is in
`ui/src/lib/icons/LICENSE-lucide`). Only the icons we use are inlined, as path data in
`ui/src/lib/icons/icons.ts`. That's about 70 icons, 12 kB before gzip, and no runtime dependency. To add
an icon, add its name to the list in that file (regenerate it from `lucide-static`, or copy the
`<svg>` body from lucide.dev), then use `<Icon name="…" />` in Svelte or `iconSvg("…")` in plain DOM
code (the editor's node views).

All icons are drawn with a 1.75 stroke, round caps and `currentColor`. They're always `aria-hidden`:
the button that holds one carries the label.

**No emoji in the UI.** Emoji are content: a callout's icon (💡 by default) is the user's, and it stays.
Moods use Lucide faces (angry, frown, meh, smile, laugh) in the mood colours.

| Where | Icon |
|---|---|
| Search | `search` |
| Today | `calendar-days` |
| Journal (its page in the tree too) | `book-open` |
| A journal entry | `calendar-days` |
| A page / a page with subpages | `file` / `file-text` |
| Settings / Trash | `settings` / `trash-2` |
| New page (tab bar) / add | `square-pen` / `plus` |
| Row menu / page actions | `ellipsis` |
| Fold, expand | `chevron-right`, turned 90° when open |
| Sidebar | `panel-left` (phone: `menu`) |
| Restore | `rotate-ccw` |
| Sync off | `cloud-off` |

## 7. Components and surfaces

### 7.1 Controls (`ui/src/lib/ui/`, `controls.css`)

| Component | Variants and states |
|---|---|
| `.btn` | default (hairline), `.primary` (accent fill), `.ghost` (no fill), `.danger`, `.danger.solid`, `.small`. Hover is `--bg-hover`, pressed is `--bg-active`, disabled is 45% opacity. |
| `.icon-btn` | Square, `--row` sized, `--text-3` at rest. On hover, `--bg-hover` and `--text-2`. Always has `aria-label`. |
| `.input` | Filled `--bg-subtle` with a hairline. On focus: `--bg`, an accent hairline and a 3 px `--focus` ring. |
| `Select.svelte` | A button and a listbox popover. Inline (quiet, in rows) or `boxed` (in forms). Keys: ↑ ↓ Home End, Enter or Space to pick, Esc, and type a letter to jump. The selected option shows a tick. |
| `Switch.svelte` | `role="switch"`, 32×18 (44×26 on touch). The accent means on. |
| `.checkbox` | 16 px, 3 px radius. Checked: an accent fill with a white tick. |
| `Menu.svelte` | A popover of `menuitem`s with an icon, label and optional shortcut, and `.danger` for destructive items. Arrow keys move, Enter runs, Esc closes and returns focus to the button. |
| `Modal.svelte` | On desktop, a centred card (`sm` 400, `md` 480, `lg` 1040×760, `palette` 640). On phones, a bottom sheet with a grabber (`phone="sheet"`) or the full screen (`phone="full"`). Esc or the backdrop closes it, and focus goes back to where it was. |
| Toast | Dark pill at the bottom centre (above the tab bar on phones). Icon, message, one action. 6 s. |
| Empty state | `.empty`: a 28 px `--text-3` icon and one line of `--text-2`. |

### 7.2 Frame (`App.svelte`, `shell.css`)

- **Sidebar**, 248 px, `--bg-sidebar`, hairline on the right. From top to bottom:
  - the Drusite mark and name, with a "close sidebar" button that appears on hover (⌘\\ toggles it);
  - Search (⌘K), Today, and Journal once there is one;
  - a "Pages" label, with + on hover;
  - the page tree and "New page";
  - Settings and Trash;
  - the sync status, pinned to the foot.
- **Tree rows** are 28 px. Indent is 16 px per level. The page icon becomes a chevron on hover when the
  page has subpages. "…" (Page options: Add a page inside, Move to Trash) and + appear on hover. The
  current page is `--bg-active` with `--text`. While dragging, a drop shows an accent line before or
  after a row, or an accent outline inside it.
- **Top bar**, 44 px, sticky. It holds the breadcrumb (ancestors, then the current page, each one a
  button), the sync status (dot and label), and "…" (Page actions).

### 7.3 Page (`PageEditor.svelte`, `Properties.svelte`, `page.css`)

- The column is 720 px, with 64 px above the title (24 on phones).
- The title is 40/700, its placeholder "Untitled" in `--text-3`.
- **Properties** are rows 34 px tall: a 160 px name column (type icon and name in `--text-2`), then the
  value, with remove (×) on hover. "Add a property" is a quiet `--text-3` row. Its form is a small
  raised card: name, type (Select), value, then Add and Cancel. Esc cancels.
- **Journal entries** show the full date (when the title isn't the date) and the mood picker: five
  36 px round buttons, grey at rest, filled with the mood colour when picked.

### 7.4 Editor (`editor/`, `editor.css`)

- Blocks have 3 px vertical padding and a 26 px indent step. Headings are 600.
- Bullets are 6 px dots: filled, then hollow, then square as the indent goes in. Numbers are tabular,
  right-aligned, with the period.
- To-dos use `.checkbox`. Done items are `--text-3` and struck through.
- Toggles use a `chevron-right` that rotates. Callouts are `--bg-subtle`, radius 8, with the user's
  emoji. Quotes have a 3 px bar in `--text`. Code is mono 13.5 on `--bg-subtle`, radius 8.
- Inline code has a hairline box and `--code-text`. Links to other pages are `--accent-text` with a
  tinted underline.
- **Slash menu** (`editor/slash.ts`): `/` at the start of a block or after a space opens it. It's a
  300 px popover of 36 px rows: an icon tile, the label, and the Markdown shortcut on the right.
  Typing filters it; ↑ ↓ Enter Tab Esc work. On a block with text, the choice goes in a new block
  below.
- **Placeholders**: "Write something, or press / for blocks" on an empty page. The empty block the
  cursor is in shows "Press / for blocks", or "Heading", "List", "To-do", "Toggle" or "Quote" for
  its kind.
- **Filter block**: a hairline card. The head shows a filter icon and a summary, with "Edit filter".
  The form is a `--bg-subtle` panel of `.input`s and styled native selects (`.select-native`), since
  the editor's node views are plain DOM.
- **Attachments**: images at radius 6. Files are a hairline card with a paperclip, the name and size.
- **Block handles** (drag to reorder, the ⋮⋮ menu) aren't drawn, because Drusite has no block
  reordering yet. When it does, they go 24 px left of the block, appear on hover, and are `--text-3`.

### 7.5 Journal views (`JournalViews.svelte`, `journal.css`)

- **Calendar**: round day cells up to 40 px. Today is filled with the accent. A day with an entry is
  600 weight with a hairline ring. Mood days are tinted with their mood at 28%. Above it: the month, then Today
  (back to this month), previous and next.
- **On this day**: rows with the entry's title, "n years ago" below it, and its mood face.
- **Mood over time**: an accent line at 60%, dots in mood colours, and mood faces on the axis. The
  range is a Select.

### 7.6 Search, trash, toasts

- **Search (⌘K)**: a 640 px palette, 12 vh from the top. It has an 18 px input with a search icon,
  then results: icon, title, "— Parent", and a snippet with highlights in `--accent-soft-2`. A footer
  shows the keys. On phones it fills the screen, with a Cancel button.
- **Trash**: a modal. Rows have an icon, title and meta, and restore and delete-forever icon buttons.
  The footer holds a count and "Empty trash", and confirms inline.

### 7.7 Settings (`Settings.svelte`, `settings.css`)

- **Desktop**: a 1040×760 modal. On the left, a 232 px nav on `--bg-sidebar`, with groups **Account**
  (Account & sync, Appearance), **Notes** (Journal, Markdown folder, Import) and **Connections** (AI,
  Agents). The section page is on the right: a 22/600 title, a one-line description, then **groups**.
  - A group has a 16/500 title over a hairline.
  - A **row** is a label (14/500) and description (13, `--text-2`) on the left, with the control on
    the right: a quiet Select, a Switch or a button.
  - Forms that open inside a section (a provider, an agent) are a `.form-card`.
- **Phones**: full screen, as a list of sections in iOS-style inset groups (uppercase 12 px labels,
  48 px rows, chevrons). Each section opens as a page with a back chevron and a centred title.
- Settings opens at Account & sync from ⌘, or the sync status, and at the list on phones.
- Markdown folder and Import appear only where the core supports them (desktops).

### 7.8 Server pages

Sign-in, account, agent consent and errors are served by `drusite-server` without the app. They use the
same tokens, trimmed, in `assets/signin.css`. A 26 rem card sits on `--bg-sidebar`, with a gem mark on
the icon's gradient, 40 px inputs and buttons, and an accent primary. On phones the card fills the
screen.

## 8. Layout and breakpoints

| Range | Shape |
|---|---|
| > 700 px (desktop, tablet landscape, iPad) | Sidebar and page. The sidebar can be put away (⌘\\). Settings is a modal. |
| ≤ 720 px | Journal views stack into one column. |
| ≤ 700 px (phones, narrow windows) | **Top bar**: menu, page title, sync dot, "…". **Tab bar**: Pages, Today, Search, New page. The sidebar is a drawer from the left (86 vw, max 320). Modals are bottom sheets. Settings and Search are full screen. Page padding is 16 px. |
| `pointer: coarse` | Rows 40 px, controls 44 px, UI text 16 px. Hover-only controls are always visible. The formatting toolbar shows above the keyboard while editing, and the tab bar steps aside. |

Every fixed bar pads by `env(safe-area-inset-*)`. The phone toolbar follows the keyboard through
`visualViewport` (iOS) and window resizes (Android).

**Phone toolbar** (Notion mobile's): + (types `/` and opens the block menu) | heading, bulleted list,
to-do, bold, italic, outdent, indent | undo, redo | hide keyboard. Its buttons are 44×40 and act on
`pointerdown`, so the keyboard stays up.

## 9. Accessibility

- **Focus**: one ring for everything, `:focus-visible` only, 2 px in `--focus` with a 1 px offset.
  Inputs show the ring as a 3 px glow. Never remove it without a replacement.
- **Keyboard**: everything works without a pointer.
  - ⌘K opens search, ⌘, opens Settings, ⌘\\ toggles the sidebar, and Esc closes the top layer.
  - Tree rows are `treeitem`s and open on Enter.
  - Menus and selects use arrow keys. The slash menu uses arrow keys, Enter and Tab.
- **Roles**: dialogs are `aria-modal` with a name. Menus are `menu`/`menuitem`, selects are
  `listbox`/`option` with `aria-activedescendant`, and the switch is `role="switch"`. The mood picker is
  a `radiogroup`. Icon-only buttons always have `aria-label`, and icons are `aria-hidden`.
- **Touch targets**: at least 36×36 for secondary controls and 44×44 for primary ones on touch
  screens.
- **Contrast**: see §2. Don't use `--text-3` for anything someone has to read to use the app, other
  than placeholders.
- **Motion**: honour `prefers-reduced-motion`.

## 10. Writing CSS

- New values go in `tokens.css` first. Components use only `var(--…)`.
- Put each rule in the stylesheet for its surface (see the table at the top). Shared pieces go in
  `controls.css`.
- Hairlines are `box-shadow: inset 0 0 0 1px var(--border)`, not borders, so state changes don't move
  layout.
- Hover-only UI needs a `@media (hover: none)` rule that shows it.
- The app's CSP is `style-src 'self'`: no inline `<style>` and no `style="…"` in markup. Svelte's
  `style:` directive and `element.style` are fine, because they go through CSSOM.
- Keep the bundle lean: no UI framework, no icon font, no web fonts.

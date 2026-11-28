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
| Ask your notes | `ui/src/styles/ask.css` |
| Share to web | `ui/src/styles/share.css` |
| Published pages (served by the server) | `crates/drusite-server/assets/publish.css` |
| Databases (views, cells, their editors) | `ui/src/styles/database.css` |
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
| Workspace switcher | One workspace, shown as the Drusite mark and name. The foot of the sidebar is the **account menu** instead: who and where, Settings, Trash, sign in or out. | Drusite has one workspace per account. Notion splits Trash and the like into a second menu beside the switcher, which is easy to miss; Drusite keeps them in one. |
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
| `--sidebar-width` | 248 px | Desktop sidebar, by default; it can be dragged from 200 to 480 px |
| `--ask-width` | 400 px | The Ask panel beside the page |
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
| Ask your notes | `sparkles` |
| Saved chats / a chat | `history` / `message-square-text` |
| Ask (send) / stop | `arrow-up` / `square` |
| Sync off | `cloud-off` |
| Published to web, Share | `globe` |
| A page's password | `lock` |

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
  - the Drusite mark and name, with a "close sidebar" button (⌘\\ toggles it);
  - Search (⌘K), Today, and Journal once there is one;
  - a "Pages" label, with + on hover;
  - the page tree and "New page";
  - the **account button**, pinned to the foot above a hairline: avatar (the email's initial on
    `--accent-soft-2`, or a person icon when signed out), the email (or "Not signed in"), the sync
    dot and a chevrons-up-down. It opens the **account menu** above it, at least as wide: a header
    with a 32 px avatar, the email, the sync dot and status, and the server, a line, then Settings (⌘,) and
    Trash, a line, and Sign out (or "Sign in to sync", which opens Settings at Account). The same
    button and menu are at the foot of the phone drawer.
- **Resizing**: the sidebar's right edge (6 px, `col-resize`) shows a 2 px `--border-strong` line on
  hover. Dragging it sets the width (200–480 px, kept per device); a drag stops at the narrowest and
  never closes the sidebar. A click without a drag closes it, as in Notion. With the focus on the edge, ← and →
  step 16 px. Closing and opening animate the column unless reduced motion is on. When closed, the
  top bar shows an "open sidebar" button.
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
  280 px popover of 30 px rows (a plain 18 px icon, the label, and the Markdown shortcut on the
  right), at most 340 px tall so most of the page stays in view, with a pinned "Close menu · esc"
  row. It opens below the line being typed, or above it when there's no room, and never covers that
  line: if neither side fits, the list gets shorter and scrolls. It follows the line as the page
  scrolls or the keyboard moves. Until something is typed, "Type to filter" shows in `--text-3`
  after the `/`. Typing filters it; ↑ ↓ Enter Tab Esc work. On a block with text, the choice goes in
  a new block below.
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
- Settings opens from ⌘, or the account menu (at the list on phones), and at Account & sync from the
  top bar's sync status or "Sign in to sync".
- Markdown folder and Import appear only where the core supports them (desktops).

### 7.9 Ask your notes (`Ask.svelte`, `ask.css`)

- **Where**: it depends on where it was opened, and desktops and the browser behave the same.
  - From the page's own top-bar button (`sparkles`, "Ask about this page"), as in AFFiNE: a right
    panel beside the page (`RightPanel.svelte`, the container for any later panel), 400 px to start,
    `--bg` with a hairline on its left, the full height of the window. Its left edge drags (or takes
    ←/→) to resize it, 300–720 px, never leaving the page under 360 px, and the width is kept. It
    stays open per device, and asks about the open page.
  - From the sidebar's "Ask" row or ⌘J: over the page at the right (`--bg-elevated`, `--shadow-lg`),
    asking all the notes. Its `maximize-2` button fills the page beside the sidebar for long chats
    (the chat keeps to 760 px), and `minimize-2` shrinks it back. A citation shrinks it so the page shows.
  - On phones, both fill the screen, from the top bar's button or the tab bar's "Ask" tab, with no
    resizing or expanding.
- **Head**: 44 px: the chat's title (or "Ask your notes"), then Chats (`history`, pressed while the list
  shows), New chat, expand or shrink (floating only) and Close. Esc closes the panel when it has the
  focus, or nothing does.
- **About this page**: opened beside a page, a chip in the question box names it (`file-text`, the
  title, an × that asks all the notes instead; "Ask this page" puts it back). Questions then keep to
  that page and the pages inside it; journal entries stay out unless Settings lets them in. Moving
  to another page moves the chip with it. Servers too old for this show no chip.
- **Messages**: questions are `--bg-subtle` bubbles at the right, radius 12. Answers are plain text at
  the left, line height 1.6, in a safe Markdown subset (paragraphs, lists, bold, italic, code): never
  HTML, images or live links, since an answer can quote any page. Until the first words, a spinning
  `loader-circle` says "Searching your notes…", then "Writing…".
- **Citations**: `[n]` becomes an 18 px accent-soft pill with the number (22 px on touch). Under a
  finished answer, the cited sources are hairline chips: the number, a `file-text` icon and the page
  title. Either opens the page at that block, which flashes as a search hit does. On phones the panel
  gives way to the page. A source whose page is gone is grey and does nothing.
- **Question box**: a raised card pinned at the bottom (`--bg-elevated`, `--shadow-sm`, radius 12): a
  growing textarea (16 px on phones) on its own row, a line plus equal padding tall so the placeholder
  sits centred; under it a bar with the page chip, the server's model as a small `--text-3` label
  (from the server's `[ask.chat]`, never named in prose), and a round accent send button, which
  becomes Stop while an answer streams. Enter asks, Shift+Enter adds a line.
- **Empty and unavailable**: a 28 px `sparkles`, a 600-weight line and `--text-2` lines below: where
  answers come from ("Answers come from your pages."), whether journal entries are in ("Change" opens
  Settings › Journal), and whether the server searches only words. When Ask can't work (not signed
  in, no chat model on the server, search off) it says why and what to do.
- **Saved chats**: rows like the sidebar's (`message-square-text`, title, a short time on the right,
  "…" on hover with Rename and Delete). Rename edits in place; Delete confirms inline.
- **Settings › Journal** has the switch "Ask your notes reads the journal", off by default, which
  syncs with the account.

### 7.10 Share to web (`Share.svelte`, `share.css`)

- **Where**: a quiet "Share" text button in the top bar, left of Ask, as in Notion. It turns
  `--accent-text` with a `globe` when the page is published. It opens a 380 px popover under the
  button, right edges lined up. On phones the button is hidden: "Share to web" in the page actions
  ("…") opens the same content as a bottom sheet. Esc, or a click outside, closes the popover and
  puts the focus back on the button.
- **Head row**: a 32 px round `globe` (`--bg-subtle`, `--accent-soft` with `--accent-text` when on),
  "Publish to web" with one line below it, and the Switch. Only editors and owners get a working
  switch; a viewer sees why not.
- **Published**: the link in a read-only mono `.input` (focus selects it) beside a primary small
  "Copy link", which says "Copied" with a tick for 2 s. Then the options, each a 44 px row with a
  label, a `--text-2` line and a Switch, all off until turned on: Include subpages, Search engines,
  Link expires (a date input appears: the last day the link works), Password (a password input
  and Save; once set, "Password set" with a `lock` and Change).
- **Foot**: "New link" (ghost, `refresh-cw`), which confirms inline ("The current link will stop
  working"), and "Unpublish" (ghost, danger).
- **Problems** (offline, not signed in, a refusal from the server) show as one `--bg-subtle` line
  with an `info` icon; the last known state stays visible beneath.
- **The tree** marks a published page with a 12 px `globe` in `--text-3` after its title.

### Published pages (`publish.css`, served by `drusite-server`)

The reader's side follows §7.3 and §7.4 with no JavaScript: the same 720 px column, title, block
styles, light and dark from the system, 16 px side padding on phones. A shared page with subpages
gets a sticky 44 px bar with the breadcrumb, and a "Pages inside" list under the page. Toggles are
`<details>`, to-dos are disabled checkboxes, code is highlighted with token colours that keep 4.5:1
on `--bg-subtle`. The password form and "Page not found" are the server-page card of §7.8.
### 7.11 Databases (`lib/database/`, `database.css`)

- **Where**: a full-page database shows its views under its title, using the window's width, with no
  document of its own. An inline one sits in a page as a block (slash menu: Database, Task board),
  under its title as a button that opens it. A row's page shows its properties under its title.
- **Bar**: the views as tabs (layout icon and name; the current one underlined in `--text`), + to add
  one; then Filter, Sort (accent when set), view settings, and a small primary **New**.
- **Table**: 34 px rows of hairline cells; headers are `--text-2` with the type icon, and open the
  property's form. The title cell shows **Open** on hover (always on touch). Numbers are tabular and
  right-aligned. Groups are sections with the group's chip and count.
- **Board**: columns 260 px on `--bg-subtle`, radius 8, headed by the group's chip and count; cards
  are `--bg` with `--shadow-sm`, the title 500 and the shown properties below. Cards drag by pointer
  (a press and hold on touch, so a swipe still scrolls): a ghost follows, the column under it gets an
  accent outline, and the card lands before the card under the pointer.
- **Calendar**: a month of days, Monday first, other months' days on `--bg-sidebar`, today's number
  on the accent; rows are small cards that drag between days (keeping their time and length).
- **List and gallery**: rows with the shown properties at the right; cards in a grid, 220 px wide.
- **Chips**: options are 20 px chips on `--tag-<colour>` (ten colours, chosen for Drusite); status chips
  are pills with a dot. People are an initial on `--accent-soft-2` and the name.
- **Editing**: a cell edits in a popover by it (a sheet on phones): a field; a searchable list of
  options where Enter makes a new one; a date with an optional end and time; people; the related
  database's rows. A property's form has its name, type, and what the type needs; a formula is
  checked as it's typed ("Gives a number", or the error in `--danger`).
- **View settings**: tabs for Layout, Properties (shown ones in order, hidden ones greyed), Filter
  (conditions joined by "and"), Sort and Group.
- **Phones** (Notion mobile's shapes): the board shows one column at a time (82 vw) and snaps between
  them; the table scrolls sideways with the title column held; popovers are sheets.

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

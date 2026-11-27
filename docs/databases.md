# Databases

Drusite's databases work as Notion's do: a database is a page, each row is a page of its own, and
views (table, board, calendar, list, gallery) show the rows with their own filter, sorts, grouping
and visible properties. Tasks are a database made from the tasks template: Status, Due, Priority
and Assignee, with a board to work from.

## How they're stored

Everything lives in the workspace document, so a view is computed without loading any page:

- A database is a page of kind `database`. Its rows are its subpages.
- Its **schema** is a set of flat keys in its tree entry: `db:p:<property>:name`, `:kind`, `:pos`
  and `:desc` for each property, `db:o:<property>:<option>` for each select or status option, and
  `db:v:<view>:<field>` for each field of each view (name, layout, filter, sorts, group, shown
  properties, calendar date, cover). Values are JSON.
- Each **row's values** are keys in the row's tree entry: `v:<property>` for one value, and
  `v:<property>#<item>` for each item of a multi-select, relation, people or files value. Who made
  and last edited a row, and when, are `createdBy`, `created`, `editedBy` and `edited`.

Because nothing is a container of its own, two devices can't each make one and lose the other's.
Concurrent edits merge fact by fact: a rename on one device and a new value on another both stay;
two devices adding a tag each keep theirs; two changing one view's filter and its sort keep both.
Two devices changing the very same field keep the last writer's. `crates/drusite-crdt/tests/database.rs`
checks this, with a property test of three devices editing at random.

Properties are named by ID, never by name, so renaming one keeps every value.

**Two-way relations** are a pair of properties. One holds the links; the other (`mirror`) holds
none and shows the rows that link to it, computed. Setting the mirror side changes the rows on the
other end. So the two sides can never disagree, whatever merges.

**Computed values** (formulas, rollups, created and edited time and person, the mirror side of a
relation) are never stored. `drusite-db` computes them when a view is shown, so they're the same on
every device and can't go stale.

## Formulas

`drusite-formula` is Notion's formula language (Formulas 2.0): a parser, a type checker that
refuses what Notion's editor refuses, and an evaluator. It has the operators, `if`/`ifs`, `let`/`lets`,
`map`/`filter`/`find`/`some`/`every` with `current` and `index`, and the text, number, date, list and
people functions. Empty values behave as Notion's do (`prop("Number") + 1` is 1 when the number is
unset; `empty(0)` is true). Numbers show to 12 decimal places, as Notion shows them.

`tests/notion_oracle.txt` holds the results Notion itself showed for about 120 formulas, on a filled
row and an empty one, read from a scratch database; every one is checked. What isn't supported is
listed in `drusite_formula::UNSUPPORTED`: styles' colours, links as links, and time zones in
`formatDate`.

## The Markdown folder

A database is a folder of rows:

```
Tasks.md                       the database: drusite-kind: database, and its schema
Tasks/Water the seedlings.md   a row: its values in frontmatter, its content below
Tasks/Buy compost.md
```

A row's file:

```markdown
---
drusite-id: 01934f6e-…
Status: In progress
Due: 2026-11-28
Priority: High
Tags: web, design
Project: "[[Garden app]]"
Estimate: 10
---
Twice a day while it is warm.
```

Values are written as a person would type them: an option's name, several joined with `, `, a date
as `2026-11-28`, `2026-11-28T14:30` or `2026-12-03 → 2026-12-05`, `true`/`false`, a number, and
`[[Row title]]` for a related row. Editing one sets it in the app; a value that can't be read (an
option the property doesn't have) is left as it was. A new `.md` file in the folder is a new row.
Computed values are written too, so search and agents can read them, and are ignored when read
back.

The database's own file carries its schema and views as a `drusite-schema` block, for reading.
Change the schema in the app or through an agent.

## Agents (MCP)

`query_database` reads a database through one of its views (filter, sorts, groups; a board's
columns are its groups), with formulas and rollups computed and each row's ID. `add_row` and
`update_row` take values by property name in the Markdown folder's form, and say what would fit
when a value doesn't. `create_database` makes one, empty or from the tasks template. `read_page`
gives a row its values as frontmatter, and `update_page` sets them. Everything is undoable.

## Importing from Notion

**From an export** (Settings › Import › Notion, or `Command::ImportNotion`): Notion's
*Markdown & CSV* export, as the `.zip` (or its `Part-N.zip`s), or the folder it unzips to. Offline,
for anyone. Pages keep their tree, links and images; databases come from their CSVs, with each row
matched to its page. The export has no property types, views, formulas or rollups, so:

- types are read off the values and names (a `Status` column is a status, with groups guessed from
  the option names; `Yes`/`No` is a checkbox; `$1,200.50` is a dollar number; `PREFIX-12` is an ID);
- formulas and rollups come in as the values they showed;
- each database gets a table and, when it has a status or select, a board;
- a pair of relations that link the same rows both ways becomes one two-way relation.

**From the API** (Settings › Import › Notion › Import from Notion's API): with an internal
integration's token, every page shared with the integration. This keeps every property's type and
options, status groups, formulas (Notion's `{{notion:block_property:…}}` references are written as
`prop("…")` again), rollups, two-way relations, unique IDs, and who made and edited each row and
when. Notion's API doesn't give views, so these get a table and a board too. The token is kept in
the Keychain on Apple devices, and elsewhere in the encrypted store (whose key the operating
system's key store holds); it's sent only to Notion and never logged.

Both report what they couldn't carry over: a formula using a function Drusite lacks, a relation to
a database that wasn't shared, a property type Drusite doesn't have (buttons, places).

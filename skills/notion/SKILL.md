---
name: notion
description: Read and write Notion through the `notion` CLI - query databases (filters, sorts, relations), read any page as markdown, and edit pages (append, update, replace, delete blocks, create pages, set properties). Use whenever the user wants to look something up in Notion, read or summarize a Notion page or database, write or restructure a Notion page, add content to one, or follow relation properties. Auth is automatic via ~/.notion_token (NOTION_TOKEN overrides); never conclude Notion is unavailable just because the env var is unset, run the command. Never hand-roll curl calls to api.notion.com; use `notion api` for anything not covered.
---

# Notion

Everything goes through the `notion` binary (on PATH via `~/.cargo/bin`). Every id
argument accepts a bare id (with or without dashes) or a Notion URL.

## Reading

```bash
notion search "Tasks"                 # databases: "<id>\t<title>"
notion search "Roadmap" --pages       # pages instead
notion schema <db>                    # "<property>\t<type>", check before filtering
notion query <db> [body] [--limit N]  # one flattened JSON object per row (_id = page id)
notion query <db> --raw               # unflattened API objects
notion page <id> [id ...]             # flatten pages by id (resolve relation ids)
notion read <page> [--depth N] [--ids]   # page content as markdown
```

`query` body is a Notion query object: inline JSON, a path to a `.json` file, `@file`
or `-`. Pagination is automatic; `--limit` stops early, so use it on big databases.

```bash
notion query <db> '{"filter":{"property":"Status","status":{"equals":"Done"}},"sorts":[{"property":"Due","direction":"ascending"}]}'
notion query <db> | jq -s '.'                                   # JSON array
notion query <db> | jq -r '[._id,.Name,.Status]|@tsv' | column -t -s$'\t'
```

**Relations** flatten to a list of related page ids. Resolve only the ones asked about,
de-duplicated:

```bash
notion query <db> | jq -r '.Project[]?' | sort -u | xargs notion page | jq -s '.'
```

A query returns at most 25 ids per relation; for more, page through
`notion api GET /pages/<id>/properties/<property_id>`.

### Filter shapes by property type
- title/rich_text: `{"property":"Name","title":{"contains":"foo"}}`
- select / status: `{"property":"P","select":{"equals":"X"}}` / `"status"`
- multi_select: `{"property":"P","multi_select":{"contains":"X"}}`
- number: `{"property":"P","number":{"greater_than":5}}`
- checkbox: `{"property":"P","checkbox":{"equals":true}}`
- date: `{"property":"P","date":{"on_or_after":"2026-01-01"}}`
- combine: `{"and":[...]}` / `{"or":[...]}`

## Writing

Content is markdown in the dialect `notion read` prints, so read → edit → write
round-trips. Markdown arguments are text, `@file` or `-` (stdin); use a file for
anything multi-line.

```bash
notion append <page> @/tmp/section.md [--after <block>]    # add blocks at the end, or after a block
notion create-page --parent <page> --title "Title" @/tmp/body.md   # prints the new URL
notion create-page --parent <db> --database --title "Row title"    # database row
notion set-props <page> "Status=Done" "Tags=a,b" "Due=2026-11-01"
```

Editing existing content: read with `--ids` first, which tags every block with
`<!-- block-id -->`, then act on those ids:

```bash
notion read <page> --ids
notion update <block> "New text for this bullet"     # same block type, text changes in place
notion replace <block> @/tmp/new.md                  # swap one block for any markdown
notion delete <block> [block ...]                    # refuses child pages unless --force
notion clear <page>                                  # delete all content, keeps child pages/databases
```

### Markdown dialect
- `#`, `##`, `###` headings; paragraphs (consecutive lines join with a line break)
- `- ` bullets, `1. ` numbered, `- [ ]` / `- [x]` to-dos; indent 2 spaces to nest
- `> quote`, `> [!💡] text` callout (any emoji), ```` ```lang ```` code, `---` divider
- pipe tables (a `|---|` row after the first row makes it a header row)
- `![caption](https://...)` image (external URL)
- `<details><summary>Title</summary>` ... `</details>` toggle; `<summary>## Title</summary>` makes a toggle heading
- inline `**bold**`, `*italic*`, `~~strike~~`, `` `code` ``, `[text](url)`, bare URLs

Writing to a shared workspace is visible to others: for anything beyond a small,
requested edit, show the user the markdown first.

## Anything else

```bash
notion api GET /users/me
notion api PATCH /blocks/<id> '{"heading_2":{"is_toggleable":true}}'
notion api POST /data_sources/<id>/query @body.json --version 2025-09-03
```

Default API version is `2022-06-28`; pass `--version` for newer endpoints.

## Errors

`Notion API error: <status> <code> <message>`: relay it. "object not found" almost
always means the page or database isn't shared with the integration (page `•••` →
Connections → add it).

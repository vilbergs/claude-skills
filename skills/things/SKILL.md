---
name: things
description: Read and write the user's Things 3 tasks on this Mac — list Today/Inbox/Upcoming/Anytime/Someday/Logbook, projects, areas, tags, search, show a to-do or project with notes and checklist; add to-dos and projects, reschedule, move, edit notes/tags/deadlines, complete, cancel, delete. Use whenever the user mentions Things, their to-dos, tasks, next actions, inbox, "what's on my plate", or asks to add/capture/complete/schedule a task. Things 3 is the user's single source of truth for actionable tasks (GTD), so also use it during daily/weekly planning alongside the calendar.
---

# Things 3

All access goes through the `things` binary (on PATH via `~/.cargo/bin`).

- **Reads** query the Things SQLite database directly (read-only, instant, works with the app closed).
- **Writes** use the official `things:///` URL scheme (auth token is read from the DB automatically); the app syncs them to Things Cloud like any manual edit. Delete uses AppleScript (moves to Trash).
- `--format json` gives structured output: `things --format json today`.
- Every item prints with its id, e.g. `(7NfxiLP5VPvp9M1N1LZQdW)`. Use ids for update/complete/delete.

## Reading

| Command | What |
|---|---|
| `things today` | Today list (incl. overdue start dates and deadlines due) |
| `things inbox` / `anytime` / `someday` / `upcoming` | Those lists (`--limit N`) |
| `things logbook --limit 20` | Recently completed/canceled |
| `things deadlines` | Open items with deadlines, soonest first |
| `things repeating` | Repeating templates |
| `things projects [--area Work]` | Open projects |
| `things areas` / `things tags` | Areas and tags |
| `things search "text" [--all]` | Title + notes search (`--all` includes done) |
| `things tagged Outside` | Open items with a tag |
| `things show <id>` | Full detail: notes, checklist, and for a project its to-dos. Also accepts an area name/id |

## Writing

```bash
things add "Book a tire change" --when today --list Personal --tags "Outside" --deadline 2026-10-20 \
       --notes "Call the shop" --checklist "Find slot" --checklist "Bring rims"
things add "Thing" --list "Some Project" --heading "Content"     # into a project/heading
things add-project "Plan Christmas" --area Family --when someday --todo "Book flights" --todo "Gifts"
things update <id> --when tomorrow        # today | tomorrow | evening | anytime | someday | YYYY-MM-DD[@HH:MM] (reminder)
things update <id> --title "New" --append-notes "more" --add-tags "Errand" --deadline ""   # "" clears
things update <id> --list Work            # move (project/area by name, or --list-id)
things update <id> --append-checklist "step 3"
things complete <id>      things cancel <id>      things update <id> --reopen
things delete <id>        # moves to Trash
things reveal <id>        # open it in the Things app
```

`add` prints `{"created": "<id>"}` once the new item appears in the DB. `update`/`complete` print the item afterwards; check it to confirm the change landed.

## Notes

- Tags passed to `--tags`/`--add-tags` must already exist (`things tags`); unknown tags are silently dropped by Things.
- `add` with no `--when`/`--list` lands in the Inbox, which is the right default for quick capture.
- Before bulk changes (many completes, moves, deletes), show the user the list and confirm. Single requested edits need no confirmation.
- If writes fail with "No URL auth token": Things → Settings → General → Enable Things URLs.

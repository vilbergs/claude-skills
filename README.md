# claude-skills

Small Rust CLIs that back my [Claude Code](https://claude.com/claude-code) skills,
plus the skills that are generic enough to share. They replace the Python, bash and
jq one-offs Claude kept rewriting: each tool is one static binary with `--help`, and
the skill's `SKILL.md` just tells Claude which subcommand to run.

| Binary | What it does | Skill |
|---|---|---|
| `notion` | Query databases, read pages as markdown, append/update/replace/delete blocks, create pages, set properties, file template-based issues, raw API calls | [`skills/notion`](skills/notion/SKILL.md) |
| `prkit` | GitHub PR helpers on top of `gh`: review threads with comment ids, post a review from a draft dir (anchors validated against the diff), replies, pending reviews, my review queue, team triage | used by private review skills |
| `things` | Things 3 on macOS: reads the local SQLite DB, writes via the `things:///` URL scheme | [`skills/things`](skills/things/SKILL.md) |
| `asc` | App Store Connect API: signs its own JWT; Xcode Cloud workflows/runs/artifacts, builds, raw calls | [`skills/app-store-connect`](skills/app-store-connect/SKILL.md) |
| `hevy` | Hevy workout tracker API: log a workout from a compact spec (`"3x8@80 rpe8"`, exercises by name), search exercise templates, list/show/update workouts | [`skills/hevy`](skills/hevy/SKILL.md) |

## Install

```bash
for c in notion prkit things asc hevy; do cargo install --path crates/$c; done
cp config.example.toml ~/.config/claude-skills/config.toml   # then fill in your ids
for s in skills/*/; do ln -s "$PWD/$s" ~/.claude/skills/; done
```

Credentials never live in this repo or the config: the Notion token is read from
`~/.notion_token` (or `NOTION_TOKEN`), GitHub auth comes from `gh`, the App Store
Connect key is an `AuthKey_<id>.p8` file outside the repo, and the Hevy API key is read
from `~/.hevy_api_key` (or `HEVY_API_KEY`).

## Layout

```
crates/common   config loading (~/.config/claude-skills/config.toml), shared helpers
crates/notion   notion CLI
crates/prkit    prkit CLI
crates/things   things CLI
crates/asc      asc CLI
crates/hevy     hevy CLI
skills/         SKILL.md files, symlinked into ~/.claude/skills
```

`cargo test` runs the markdown ↔ Notion block tests and the Hevy workout spec tests.

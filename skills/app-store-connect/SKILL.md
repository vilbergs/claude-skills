---
name: app-store-connect
description: Query the App Store Connect API with the `asc` CLI - Xcode Cloud workflows, build runs, build actions with errors/test failures and artifacts (logs, xcresult, archives), TestFlight/App Store builds, apps, and any other endpoint. Use when the user asks about an Xcode Cloud build, CI status of an iOS/macOS app, why a build failed, build numbers, TestFlight builds, beta groups, or anything in App Store Connect. Signs its own JWT from an AuthKey_*.p8 file, so no Python, venv or /tmp client is needed.
---

# App Store Connect

`asc` (on PATH via `~/.cargo/bin`) signs a short-lived ES256 JWT per call from an API
key. Key id and issuer come from `--key`/`--issuer`, `ASC_KEY_ID`/`ASC_ISSUER_ID`, or
`[asc]` in `~/.config/claude-skills/config.toml`. The `AuthKey_<id>.p8` file is found
in the configured `key_dir`, `~/.appstoreconnect/private_keys`, `~/.private_keys` or
`~/private_keys`. Never print or copy the .p8 contents.

## Xcode Cloud

```bash
asc workflows                          # products and their workflows (ids)
asc runs <workflow-id> [--limit 5]     # recent runs: number, status, commit
asc run <run-id> [--urls]              # actions with errors/test failures/warnings, artifacts
```

To debug a failed run: `asc run <id> --urls`, download the `LOG_BUNDLE` or
`RESULT_BUNDLE` artifact with `curl -L -o`, unzip, and read the logs. The URLs are
signed and expire, so fetch them right before downloading.

## Builds and apps

```bash
asc apps                               # "<app id>\t<bundle id>\t<name>"
asc builds <app-id> [--version 97]     # newest first, processing state
```

## Anything else

```bash
asc get /v1/apps/<id>/betaGroups
asc get "/v1/builds?filter[app]=<id>&limit=200" --all     # follow pagination
asc patch /v1/builds/<id> @body.json
asc post /v1/... '{"data":{...}}'
```

Reference: https://developer.apple.com/documentation/appstoreconnectapi. Errors print as
`HTTP <code> <title>: <detail>`. A 401 usually means a wrong issuer/key pair, and a 403
means the key's role lacks access.

Changing things in App Store Connect (builds, groups, submissions) is outward-facing:
confirm with the user before any POST/PATCH/DELETE.

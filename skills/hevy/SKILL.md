---
name: hevy
description: Work with the user's Hevy workout tracker through the `hevy` CLI - log workouts from a plain description ("3x8 bench at 80, then pull-ups"), repeat or tweak a past workout, log a routine as done, build and edit routines and programs with rep ranges and rest, review progress on a lift (best sets, estimated 1RM, PRs, trend), summarize training over a period (frequency, volume, sets per muscle group), and log or read body weight and measurements. Use whenever the user mentions Hevy, wants to log/record a workout, gym session, lift or run, asks how a lift is progressing or what they've been training, wants a routine or training plan put into Hevy, or logs their body weight.
---

# Hevy

`hevy` (on PATH via `~/.cargo/bin`) talks to the Hevy public API (needs Hevy Pro). The
API key comes from `HEVY_API_KEY` or `~/.hevy_api_key` (`[hevy] api_key_file` in
`~/.config/claude-skills/config.toml`). Never print the key. If a call fails with HTTP
401, the user needs a key from https://hevy.com/settings?developer.

Weights: bare numbers in specs and all output use `[hevy] units` (`kg` default, or
`lb`); `--units kg|lb` overrides per call. `hevy user` shows the unit the user picked
in the app; if it differs from the config, pass `--units` to match.

Workouts and routines are written from JSON specs with set shorthand like
`"3x8@80 rpe8"`; the full format is in [spec.md](spec.md). Read it before writing a
spec.

## Logging a workout

1. Write a workout spec from what the user said. Ask only for what you can't infer:
   exercises and sets are required; time defaults to "ended now, lasted an hour".
2. `hevy create --dry-run @spec.json` resolves every exercise to a template and prints
   the workout as it would be logged. Fix names it can't resolve (below).
3. Show the user the preview and create it once it's right: `hevy create @spec.json`.

There is no delete in the API. A wrong workout can only be replaced with
`hevy update <id> @spec.json` or deleted by the user in the app, so always dry-run
first and never create the same workout twice. After an unclear error, check
`hevy workouts` before retrying.

Starting points instead of writing from scratch:

```bash
hevy spec workout last > spec.json              # "same as last time", then edit
hevy spec workout <id> > spec.json              # any past workout
hevy spec routine "Push A" --as-workout > spec.json   # "did my Push A routine"
```

`spec` prints times from the source workout: change `start` to the new session (or drop
it plus `end` and set `duration`). `--as-workout` turns rep ranges into their low end
and says which exercises it changed; put in the reps the user actually did.

## Routines (plans)

```bash
hevy routines                                    # id, title, folder, exercise count
hevy routine "Push A"                            # by title or id
hevy folders            hevy add-folder "PPL"
hevy create-routine --dry-run @routine.json      # then without --dry-run
hevy spec routine "Push A" > routine.json        # edit, then:
hevy update-routine "Push A" @routine.json --dry-run
```

`update-routine` replaces the whole routine. For a multi-day program, create a folder
and one routine per day. Base starting weights on `hevy history` instead of guessing.
Creating or changing routines writes to the user's account: show the dry run and
confirm first.

## Progress and review

```bash
hevy history bench press barbell [--since 12w]   # per session: sets, best set, e1RM, volume; PRs; trend
hevy stats [--since 30d]                         # workouts/week, time, sets per muscle, volume, top exercises
hevy workouts --limit 20       hevy workout <id|last>
```

`--since` takes `30d`, `12w`, `6mo`, `1y` or a date. Warm-up sets never count. e1RM is
Epley (`w × (1 + reps/30)`) on the logged weight. For weighted bodyweight exercises
that is the added load, so compare those by reps and load, not e1RM. Use these for
questions like "is my squat going up", "what did I train this month", "am I neglecting
legs", or before suggesting next week's weights: progress a lift when its recent best
sets hit the target reps, and hold or back off when e1RM has stalled or dropped.

## Body measurements

```bash
hevy body [--limit 10]
hevy measure weight=80.5 fat=18.5 waist=82 [--date 2026-10-19]
```

`weight`/`lean` use the default unit, `fat` is %, the rest are cm (`waist`, `hips`,
`chest`, `neck`, `shoulder`, `left_bicep`, `right_thigh`, ...). Logging a date that
already has an entry merges the new values into it.

## Exercises

```bash
hevy exercises bench barbell        # id, title, type, equipment, primary muscle
hevy exercises --muscle lats        hevy exercises --custom
hevy exercises --refresh row        # refetch the library (cached for a week)
```

When a spec name is ambiguous, the error lists the candidates: pick the user's usual
variant (check their past workouts) or ask. If nothing fits, ask before creating a
custom exercise. It's permanent and counts toward Hevy's custom exercise limit:

```bash
hevy add-exercise "Landmine Press" --type weight_reps --equipment barbell --muscle shoulders --other triceps
```

## Anything else

`hevy get|post|put <path> [body]` for the rest of the API, e.g.
`/v1/workouts/events?since=2026-10-01T00:00:00Z` (workouts changed or deleted since a
date). Spec: https://api.hevyapp.com/docs.

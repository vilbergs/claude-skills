---
name: hevy
description: Log workouts to the user's Hevy account with the `hevy` CLI - turn "I did 3x8 bench at 80 and some pull-ups" into a Hevy workout with the right exercise templates, sets, weights, RPE, supersets and times; also search Hevy's exercise library, create custom exercises, list and show recent workouts, and fix a logged workout. Use whenever the user mentions Hevy, wants to log/record/save a workout, gym session, lift, run or training session, or asks what they did in a recent workout.
---

# Hevy

`hevy` (on PATH via `~/.cargo/bin`) talks to the Hevy public API (needs Hevy Pro). The
API key comes from `HEVY_API_KEY` or `~/.hevy_api_key` (`[hevy] api_key_file` in
`~/.config/claude-skills/config.toml`). Never print the key. `[hevy] units = "lb"` makes
bare weights pounds; `--units kg|lb` overrides per call. If a call fails with HTTP 401,
the user needs a key from https://hevy.com/settings?developer.

## Creating a workout

1. Write a spec (below) from what the user said. Ask only for what you can't infer:
   exercises and sets are required; time defaults to "ended now, lasted an hour".
2. `hevy create --dry-run @/path/spec.json` (or the JSON inline / `-` for stdin). It
   resolves every exercise name to a template and prints the workout as it would be
   logged. Fix any name it can't resolve (see below).
3. Show the user the preview and create it once it's right: `hevy create @spec.json`.
   It prints the workout as Hevy stored it, with its id.

The API has no delete. A wrong workout can only be fixed with
`hevy update <id> @spec.json` (replaces it entirely) or deleted by the user in the app,
so always dry-run first and never create the same workout twice — on an unclear error,
check `hevy workouts` before retrying.

### Spec

```json
{
  "title": "Push day",
  "description": "Felt strong",
  "start": "2026-10-19 17:30",
  "duration": "1h10m",
  "private": false,
  "exercises": [
    {"exercise": "Bench Press (Barbell)", "notes": "paused reps",
     "sets": ["w 10@40", "3x8@80 rpe8", "f 6@80"]},
    {"exercise": "Pull Up", "superset": 0, "sets": "3x10"},
    {"exercise": "Plank", "superset": 0, "sets": ["3x60s"]},
    {"exercise": "Running", "sets": [{"km": 5, "duration": "27:30"}]}
  ]
}
```

- Only `title` and `exercises` are required. Times: `start`/`end` are RFC 3339,
  local `"YYYY-MM-DD HH:MM"` or `"HH:MM"` (today); `duration` is `"1h15m"`, `"75m"`,
  `"1:15"` or minutes. Any two of start/end/duration work; missing end means now,
  missing duration means 1h. "This morning" etc. — pick a sensible local time.
- `exercise`: a template title or id. Matching ignores case and punctuation, and a
  partial name works when it's unique ("incline bench" ok, "bench" is ambiguous).
- `superset`: exercises with the same number are one superset.
- `sets`: one shorthand string, or a list of strings and objects, in order.

Set shorthand, space-separated tokens:

| Token | Meaning |
|---|---|
| `3x...` | repeat the set 3 times (`3x8@80`, `w 2x10@40`) |
| `8` | reps |
| `@80`, `@80kg`, `@175lb`, `80kg` | weight; bare numbers use the default unit |
| `@bw` | bodyweight, no added weight |
| `5km`, `400m`, `2mi` | distance |
| `60s`, `2min`, `1:30`, `1:02:03` | duration |
| `rpe8`, `rpe9.5` | RPE: 6, 7, 7.5, 8, 8.5, 9, 9.5 or 10 |
| `w`, `f`, `d` (`warmup`, `failure`, `dropset`) | set type; default normal |

Set objects take `reps`, `kg`, `lb`, `weight` (default unit), `km`, `m`, `mi`,
`seconds`, `duration` ("1:30"), `rpe`, `type`, `count`, `custom_metric` (steps/floors),
or the raw API names (`weight_kg`, `distance_meters`, `duration_seconds`).

Match the fields to the exercise's type from `hevy exercises`: `weight_reps` wants
weight and reps, `bodyweight_reps`/`reps_only` reps, `duration` a time,
`distance_duration` distance and time, `short_distance_weight` distance and weight.
For `bodyweight_assisted_reps` the weight is the assistance.

### Finding exercises

```bash
hevy exercises bench barbell        # id, title, type, equipment, primary muscle
hevy exercises --muscle lats
hevy exercises --custom             # the user's own exercises
hevy exercises --refresh row        # refetch the library (cached for a week)
```

When a name is ambiguous, pick the obvious one (the user's usual variant shows in
`hevy workouts`/`hevy workout <id>`) or ask. If nothing fits, ask before creating a
custom exercise — it's permanent and counts toward Hevy's custom exercise limit:

```bash
hevy add-exercise "Landmine Press" --type weight_reps --equipment barbell --muscle shoulders --other triceps
```

## Reading

```bash
hevy workouts [--limit 10]          # id, start, title, exercise count; newest first
hevy workout <id> [--json]          # exercises and sets
hevy user                           # checks the key
```

To log "the same as last Tuesday" or progress a lift, read the earlier workout and
build the new spec from it.

## Anything else

`hevy get|post|put <path> [body]` for the rest of the API: routines
(`/v1/routines`, `/v1/routine_folders`), `/v1/exercise_history/<template-id>`,
`/v1/workouts/events?since=...`, `/v1/body_measurements`. Spec:
https://api.hevyapp.com/docs. Creating or changing routines is a write to the user's
account like a workout: confirm first.

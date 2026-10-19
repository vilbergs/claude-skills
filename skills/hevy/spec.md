# Hevy spec format

`hevy create`/`update` take a workout spec and `hevy create-routine`/`update-routine` a
routine spec, as JSON text, `@file` or `-` (stdin). `hevy spec workout|routine <id>`
prints an existing one in this format.

## Workout spec

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

- Only `title` and `exercises` are required. `start`/`end` are RFC 3339, local
  `"YYYY-MM-DD HH:MM"` or `"HH:MM"` (today); `duration` is `"1h15m"`, `"75m"`, `"1:15"`
  or minutes. Any two of start/end/duration work; missing end means now, missing
  duration means 1h.
- `exercise`: a template title or id. Matching ignores case and punctuation, and a
  partial name works when it's unique ("incline bench" ok, "bench" is ambiguous).
- `superset`: exercises with the same number are one superset.
- `sets`: one shorthand string, or a list of strings and objects, in order.

## Routine spec

```json
{
  "title": "Push A",
  "folder": "PPL",
  "notes": "Add 2.5 kg when every set hits the top of the range",
  "exercises": [
    {"exercise": "Bench Press (Barbell)", "rest": "2min", "sets": ["w 10@40", "3x8-12@60"]},
    {"exercise": "Lateral Raise (Dumbbell)", "rest": 60, "sets": "3x12-15@8"}
  ]
}
```

Same exercise fields as a workout plus `rest` (`"90s"`, `"2min"`, `"1:30"` or
seconds). Sets may use rep ranges (`8-12`); routines take no RPE (put targets in the
notes) and no times. `folder` is a folder title or id; leave it out for "My Routines".
On `update-routine`, leaving `folder` out keeps the routine where it is.

## Set shorthand

Space-separated tokens:

| Token | Meaning |
|---|---|
| `3x...` | repeat the set 3 times (`3x8@80`, `w 2x10@40`) |
| `8` | reps |
| `8-12` | rep range (routines only) |
| `@80`, `@80kg`, `@175lb`, `80kg` | weight; bare numbers use the default unit |
| `@bw` | bodyweight, no added weight |
| `5km`, `400m`, `2mi` | distance |
| `60s`, `2min`, `1:30`, `1:02:03` | duration |
| `rpe8`, `rpe9.5` | RPE: 6, 7, 7.5, 8, 8.5, 9, 9.5 or 10 (workouts only) |
| `w`, `f`, `d` (`warmup`, `failure`, `dropset`) | set type; default normal |

Set objects take `reps`, `rep_range` (`[8, 12]` or `"8-12"`), `kg`, `lb`, `weight`
(default unit), `km`, `m`, `mi`, `seconds`, `duration` (`"1:30"`), `rpe`, `type`,
`count`, `custom_metric` (steps/floors), or the raw API names (`weight_kg`,
`distance_meters`, `duration_seconds`).

Match the fields to the exercise's type (`hevy exercises` shows it): `weight_reps`
wants weight and reps, `bodyweight_reps`/`reps_only` reps, `duration` a time,
`distance_duration` distance and time, `short_distance_weight` distance and weight.
For `bodyweight_assisted_reps` the weight is the assistance.

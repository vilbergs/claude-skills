//! hevy — the Hevy workout tracker's public API from the terminal.
//!
//! Auth: an API key from https://hevy.com/settings?developer (Hevy Pro), read from
//! HEVY_API_KEY or the file in `[hevy] api_key_file` (default `~/.hevy_api_key`).
//! Exercise templates are cached in ~/.cache/claude-skills/hevy-exercises.json so names in
//! a spec resolve without paging through the whole library on every call.

mod api;
mod spec;
mod stats;

use anyhow::{Context, Result, anyhow, bail};
use api::{Hevy, HttpError};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};
use spec::{Match, RoutineSpec, Spec, Unit, num};

#[derive(Parser)]
#[command(name = "hevy", version, about)]
struct Cli {
    /// Unit for bare weights in specs and for output (kg or lb); default from [hevy] units
    #[arg(long, global = true)]
    units: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Workout,
    Routine,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a workout from a spec (JSON text, @file or -); see the skill for the format
    Create {
        spec: String,
        /// Resolve exercises and print the workout without creating it
        #[arg(long)]
        dry_run: bool,
        /// Also print the request body
        #[arg(long)]
        json: bool,
    },
    /// Replace an existing workout with a spec (same format as create)
    Update {
        id: String,
        spec: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Recent workouts, newest first
    Workouts {
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// One workout with its exercises and sets; "last" for the latest
    Workout {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print an existing workout or routine as an editable spec (workout id may be "last")
    Spec {
        kind: Kind,
        /// Workout id, or routine id or title
        id: String,
        /// For a routine: a workout spec to log it (rest dropped, rep ranges at their low end)
        #[arg(long)]
        as_workout: bool,
    },
    /// Routines with their folder
    Routines,
    /// One routine by id or title
    Routine {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a routine from a routine spec
    CreateRoutine {
        spec: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Replace an existing routine (id or title) with a routine spec
    UpdateRoutine {
        id: String,
        spec: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Routine folders
    Folders,
    /// Create a routine folder
    AddFolder { title: String },
    /// One exercise's sessions: best set, estimated 1RM, volume, PRs and trend
    History {
        /// Exercise title or template id
        exercise: Vec<String>,
        /// 30d, 12w, 6mo, 1y or YYYY-MM-DD; default all time
        #[arg(long)]
        since: Option<String>,
    },
    /// Training summary: frequency, time, sets per muscle group, volume, top exercises
    Stats {
        #[arg(long, default_value = "30d")]
        since: String,
    },
    /// Recent body measurements, newest first
    Body {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Log body measurements: `hevy measure weight=80.5 fat=18 waist=82`; merges into an
    /// existing entry for the date
    Measure {
        /// weight, lean (default unit), fat (%), or API names like waist, chest_cm
        fields: Vec<String>,
        /// YYYY-MM-DD; default today
        #[arg(long)]
        date: Option<String>,
    },
    /// Search exercise templates by words of the title: `hevy exercises bench barbell`
    Exercises {
        query: Vec<String>,
        /// Only this primary muscle group (chest, quadriceps, lats, ...)
        #[arg(long)]
        muscle: Option<String>,
        /// Only custom exercises
        #[arg(long)]
        custom: bool,
        /// Refetch the template library instead of using the cache
        #[arg(long)]
        refresh: bool,
    },
    /// Create a custom exercise template
    AddExercise {
        title: String,
        /// weight_reps, reps_only, bodyweight_reps, bodyweight_assisted_reps, duration,
        /// weight_duration, distance_duration, short_distance_weight
        #[arg(long = "type", default_value = "weight_reps")]
        kind: String,
        /// none, barbell, dumbbell, kettlebell, machine, plate, resistance_band, suspension, other
        #[arg(long, default_value = "none")]
        equipment: String,
        /// Primary muscle group
        #[arg(long)]
        muscle: String,
        /// Secondary muscle groups (repeatable)
        #[arg(long)]
        other: Vec<String>,
    },
    /// Account info (checks the API key)
    User,
    /// GET a path like /v1/routines?page=1; prints JSON
    Get { path: String },
    /// POST a JSON body (text, @file or -)
    Post { path: String, body: String },
    /// PUT a JSON body (text, @file or -)
    Put { path: String, body: String },
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("-")
}

fn body(arg: &str) -> Result<Value> {
    serde_json::from_str(&common::read_arg(arg)?).context("body is not valid JSON")
}

fn pretty(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn local(t: &str) -> String {
    DateTime::parse_from_rfc3339(t).map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|_| t.to_string())
}

fn fmt_set(set: &Value, unit: Unit) -> String {
    let mut parts = Vec::new();
    if let Some(kg) = set["weight_kg"].as_f64() {
        parts.push(format!("{} {}", num(unit.kg_to(kg)), unit.name()));
    }
    let reps = match (set["rep_range"]["start"].as_f64(), set["rep_range"]["end"].as_f64(), set["reps"].as_f64()) {
        (Some(a), Some(b), _) => Some(format!("{}-{}", num(a), num(b))),
        (_, _, Some(r)) => Some(num(r)),
        _ => None,
    };
    if let Some(r) = reps {
        parts.push(if parts.is_empty() { format!("{r} reps") } else { format!("× {r}") });
    }
    if let Some(m) = set["distance_meters"].as_f64() {
        parts.push(if m >= 1000.0 { format!("{} km", num(m / 1000.0)) } else { format!("{} m", num(m)) });
    }
    if let Some(d) = set["duration_seconds"].as_f64().map(|d| d as u64) {
        parts.push(if d >= 3600 { format!("{}:{:02}:{:02}", d / 3600, d / 60 % 60, d % 60) } else { format!("{}:{:02}", d / 60, d % 60) });
    }
    if let Some(c) = set["custom_metric"].as_f64() {
        parts.push(format!("custom {}", num(c)));
    }
    if let Some(r) = set["rpe"].as_f64() {
        parts.push(format!("rpe {}", num(r)));
    }
    let ty = s(&set["type"]);
    if ty != "normal" && ty != "-" {
        parts.insert(0, format!("[{ty}]"));
    }
    if parts.is_empty() { "(empty)".into() } else { parts.join(" ") }
}

fn print_exercises(w: &Value, unit: Unit) {
    for (i, e) in w["exercises"].as_array().into_iter().flatten().enumerate() {
        let ss = e["superset_id"].as_u64().map(|n| format!("  [superset {n}]")).unwrap_or_default();
        let rest = e["rest_seconds"].as_u64().map(|r| format!("  [rest {r}s]")).unwrap_or_default();
        println!("{}. {}{ss}{rest}", i + 1, e["title"].as_str().unwrap_or_else(|| s(&e["exercise_template_id"])));
        if let Some(n) = e["notes"].as_str().filter(|n| !n.is_empty()) {
            println!("   {n}");
        }
        for set in e["sets"].as_array().into_iter().flatten() {
            println!("   - {}", fmt_set(set, unit));
        }
    }
}

fn print_workout(w: &Value, unit: Unit) {
    let id = w["id"].as_str().map(|id| format!(" ({id})")).unwrap_or_default();
    println!("{}{id}", s(&w["title"]));
    let (a, b) = (s(&w["start_time"]), s(&w["end_time"]));
    let mins = match (DateTime::parse_from_rfc3339(a), DateTime::parse_from_rfc3339(b)) {
        (Ok(a), Ok(b)) => format!(" ({} min)", (b - a).num_minutes()),
        _ => String::new(),
    };
    println!("{} – {}{mins}{}", local(a), local(b), if w["is_private"] == true { ", private" } else { "" });
    if let Some(d) = w["description"].as_str().filter(|d| !d.is_empty()) {
        println!("{d}");
    }
    print_exercises(w, unit);
}

fn print_routine(r: &Value, unit: Unit) {
    let id = r["id"].as_str().map(|id| format!(" ({id})")).unwrap_or_default();
    let folder = r["folder_id"].as_u64().map(|f| format!(", folder {f}")).unwrap_or_default();
    println!("{}{id}{folder}", s(&r["title"]));
    if let Some(n) = r["notes"].as_str().filter(|n| !n.is_empty()) {
        println!("{n}");
    }
    print_exercises(r, unit);
}

/// Resolves exercise names against the template library, refetching it once on a miss
/// (for templates created since the cache was written).
struct Resolver<'a> {
    hevy: &'a Hevy,
    templates: Vec<Value>,
    refreshed: bool,
}

impl<'a> Resolver<'a> {
    fn new(hevy: &'a Hevy) -> Result<Self> {
        Ok(Self { hevy, templates: hevy.templates(false)?, refreshed: false })
    }

    fn resolve(&mut self, name: &str) -> Result<String> {
        loop {
            let pairs = self.templates.iter().map(|t| (t["id"].as_str().unwrap_or(""), t["title"].as_str().unwrap_or("")));
            match spec::find_template(name, pairs) {
                Match::One(id) => return Ok(id),
                Match::Many(c) => bail!("{name:?} matches several exercises; use the exact title or id:\n  {}", c.join("\n  ")),
                Match::None if !self.refreshed => {
                    self.templates = self.hevy.templates(true)?;
                    self.refreshed = true;
                }
                Match::None => bail!("no exercise matches {name:?}; search with `hevy exercises <words>` or create it with `hevy add-exercise`"),
            }
        }
    }

    /// The body with template titles filled in, for a dry-run preview.
    fn preview(&self, mut v: Value) -> Value {
        for e in v["exercises"].as_array_mut().into_iter().flatten() {
            if let Some(t) = self.templates.iter().find(|t| t["id"] == e["exercise_template_id"]) {
                e["title"] = json!(format!("{} ({})", s(&t["title"]), s(&t["id"])));
            }
        }
        v
    }
}

/// POST/PUT/GET answer with `{"workout": [w]}`, `{"workout": w}` or `w`.
fn unwrap<'v>(r: &'v Value, key: &str) -> &'v Value {
    match &r[key] {
        Value::Array(a) if !a.is_empty() => &a[0],
        Value::Null => r,
        w => w,
    }
}

fn workout(hevy: &Hevy, id: &str) -> Result<Value> {
    let id = if id == "last" {
        let r = hevy.get("/v1/workouts?page=1&pageSize=1")?;
        r["workouts"][0]["id"].as_str().ok_or_else(|| anyhow!("no workouts yet"))?.to_string()
    } else {
        id.to_string()
    };
    Ok(unwrap(&hevy.get(&format!("/v1/workouts/{id}"))?, "workout").clone())
}

fn all_routines(hevy: &Hevy) -> Result<Vec<Value>> {
    hevy.pages("/v1/routines", "routines", 10, |_| true)
}

fn all_folders(hevy: &Hevy) -> Result<Vec<Value>> {
    hevy.pages("/v1/routine_folders", "routine_folders", 10, |_| true)
}

/// A routine by id, else by title.
fn routine(hevy: &Hevy, q: &str) -> Result<Value> {
    match hevy.get(&format!("/v1/routines/{q}")) {
        Ok(r) if !unwrap(&r, "routine")["id"].is_null() => return Ok(unwrap(&r, "routine").clone()),
        Err(e) if !matches!(e.downcast_ref::<HttpError>(), Some(h) if h.code == 404 || h.code == 400) => return Err(e),
        _ => {}
    }
    let routines = all_routines(hevy)?;
    match spec::find_template(q, routines.iter().map(|r| (r["id"].as_str().unwrap_or(""), r["title"].as_str().unwrap_or("")))) {
        Match::One(id) => Ok(routines.into_iter().find(|r| r["id"] == id.as_str()).unwrap()),
        Match::Many(c) => bail!("{q:?} matches several routines:\n  {}", c.join("\n  ")),
        Match::None => bail!("no routine matches {q:?}; `hevy routines` lists them"),
    }
}

fn folder_id(hevy: &Hevy, title: &str) -> Result<u64> {
    let folders = all_folders(hevy)?;
    let ids: Vec<(String, String)> = folders.iter().map(|f| (f["id"].to_string(), s(&f["title"]).to_string())).collect();
    match spec::find_template(title, ids.iter().map(|(a, b)| (a.as_str(), b.as_str()))) {
        Match::One(id) => Ok(id.parse()?),
        Match::Many(c) => bail!("{title:?} matches several folders:\n  {}", c.join("\n  ")),
        Match::None => bail!("no routine folder {title:?}; create it with `hevy add-folder`"),
    }
}

fn build_workout(r: &mut Resolver, arg: &str, unit: Unit) -> Result<Value> {
    let spec: Spec = serde_json::from_str(&common::read_arg(arg)?).context("reading the workout spec")?;
    spec::build(&spec, unit, Utc::now(), |name| r.resolve(name))
}

fn build_routine(hevy: &Hevy, r: &mut Resolver, arg: &str, unit: Unit) -> Result<Value> {
    let spec: RoutineSpec = serde_json::from_str(&common::read_arg(arg)?).context("reading the routine spec")?;
    spec::build_routine(&spec, unit, |name| r.resolve(name), |f| folder_id(hevy, f))
}

/// Body measurement fields from `key=value` args; weights in the default unit.
fn measurement_fields(args: &[String], unit: Unit) -> Result<Map<String, Value>> {
    const CM: [&str; 15] = [
        "neck_cm", "shoulder_cm", "chest_cm", "left_bicep_cm", "right_bicep_cm", "left_forearm_cm", "right_forearm_cm",
        "abdomen", "waist", "hips", "left_thigh", "right_thigh", "left_calf", "right_calf", "fat_percent",
    ];
    let mut m = Map::new();
    for a in args {
        let (k, v) = a.split_once('=').ok_or_else(|| anyhow!("{a:?}: use key=value, e.g. weight=80.5"))?;
        let x: f64 = v.trim().parse().map_err(|_| anyhow!("{a:?}: {v:?} is not a number"))?;
        let (key, val) = match k.trim() {
            "weight" => ("weight_kg".to_string(), unit.to_kg(x)),
            "lean" => ("lean_mass_kg".to_string(), unit.to_kg(x)),
            "fat" => ("fat_percent".to_string(), x),
            "weight_kg" | "lean_mass_kg" => (k.trim().to_string(), x),
            k if CM.contains(&k) => (k.to_string(), x),
            k if CM.contains(&format!("{k}_cm").as_str()) => (format!("{k}_cm"), x),
            k => bail!("unknown measurement {k:?}: weight, lean, fat, or one of {}", CM.join(", ")),
        };
        m.insert(key, val.into());
    }
    if m.is_empty() {
        bail!("nothing to log: pass e.g. weight=80.5");
    }
    Ok(m)
}

fn fmt_measurement(b: &Value, unit: Unit) -> String {
    let mut parts = vec![s(&b["date"]).to_string()];
    for (k, v) in b.as_object().into_iter().flatten() {
        let Some(x) = v.as_f64() else { continue };
        parts.push(match k.as_str() {
            "weight_kg" => format!("weight {} {}", num(unit.kg_to(x)), unit.name()),
            "lean_mass_kg" => format!("lean {} {}", num(unit.kg_to(x)), unit.name()),
            "fat_percent" => format!("fat {}%", num(x)),
            k => format!("{} {} cm", k.trim_end_matches("_cm"), num(x)),
        });
    }
    parts.join("  ")
}

fn main() {
    common::reset_sigpipe();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("hevy: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let unit = match cli.units.or(common::load()?.hevy.units) {
        Some(u) => Unit::parse(&u)?,
        None => Unit::Kg,
    };
    let hevy = Hevy::new()?;
    match cli.cmd {
        Cmd::Create { spec, dry_run, json } | Cmd::Update { spec, dry_run, json, .. } if dry_run => {
            let mut r = Resolver::new(&hevy)?;
            let body = build_workout(&mut r, &spec, unit)?;
            print_workout(&r.preview(body["workout"].clone()), unit);
            if json {
                pretty(&body);
            }
            println!("(dry run: nothing sent)");
        }
        Cmd::Create { spec, json, .. } => {
            let body = build_workout(&mut Resolver::new(&hevy)?, &spec, unit)?;
            if json {
                pretty(&body);
            }
            let r = hevy.call("POST", "/v1/workouts", Some(&body))?;
            println!("created:");
            print_workout(unwrap(&r, "workout"), unit);
        }
        Cmd::Update { id, spec, json, .. } => {
            let body = build_workout(&mut Resolver::new(&hevy)?, &spec, unit)?;
            if json {
                pretty(&body);
            }
            let r = hevy.call("PUT", &format!("/v1/workouts/{id}"), Some(&body))?;
            println!("updated:");
            print_workout(unwrap(&r, "workout"), unit);
        }
        Cmd::Workouts { limit } => {
            let mut seen = 0;
            let ws = hevy.pages("/v1/workouts", "workouts", 10, |page| {
                seen += page.len();
                seen < limit
            })?;
            for w in ws.iter().take(limit) {
                let n = w["exercises"].as_array().map_or(0, |e| e.len());
                println!("{}\t{}\t{}\t{n} exercises", s(&w["id"]), local(s(&w["start_time"])), s(&w["title"]));
            }
        }
        Cmd::Workout { id, json } => {
            let w = workout(&hevy, &id)?;
            if json { pretty(&w) } else { print_workout(&w, unit) }
        }
        Cmd::Spec { kind: Kind::Workout, id, .. } => pretty(&spec::export_workout(&workout(&hevy, &id)?, unit)),
        Cmd::Spec { kind: Kind::Routine, id, as_workout } => {
            let (spec, collapsed) = spec::export_routine(&routine(&hevy, &id)?, unit, as_workout);
            pretty(&spec);
            if !collapsed.is_empty() {
                eprintln!("note: rep ranges set to their low end for {}; put in the reps actually done", collapsed.join(", "));
            }
        }
        Cmd::Routines => {
            let folders = all_folders(&hevy)?;
            for r in all_routines(&hevy)? {
                let folder = folders.iter().find(|f| f["id"] == r["folder_id"]).map_or("My Routines", |f| s(&f["title"]));
                let n = r["exercises"].as_array().map_or(0, |e| e.len());
                println!("{}\t{}\t{folder}\t{n} exercises", s(&r["id"]), s(&r["title"]));
            }
        }
        Cmd::Routine { id, json } => {
            let r = routine(&hevy, &id)?;
            if json { pretty(&r) } else { print_routine(&r, unit) }
        }
        Cmd::CreateRoutine { spec, dry_run, json } => {
            let mut res = Resolver::new(&hevy)?;
            let body = build_routine(&hevy, &mut res, &spec, unit)?;
            if dry_run {
                print_routine(&res.preview(body["routine"].clone()), unit);
            }
            if json {
                pretty(&body);
            }
            if dry_run {
                println!("(dry run: nothing sent)");
            } else {
                let r = hevy.call("POST", "/v1/routines", Some(&body))?;
                println!("created:");
                print_routine(unwrap(&r, "routine"), unit);
            }
        }
        Cmd::UpdateRoutine { id, spec, dry_run, json } => {
            let target = routine(&hevy, &id)?;
            let mut res = Resolver::new(&hevy)?;
            let mut body = build_routine(&hevy, &mut res, &spec, unit)?;
            // No folder in the spec means leave it where it is, not move it to "My Routines".
            if let Some(r) = body["routine"].as_object_mut()
                && r["folder_id"].is_null()
            {
                r.remove("folder_id");
            }
            if dry_run {
                print_routine(&res.preview(body["routine"].clone()), unit);
            }
            if json {
                pretty(&body);
            }
            if dry_run {
                println!("(dry run: nothing sent)");
            } else {
                let r = hevy.call("PUT", &format!("/v1/routines/{}", s(&target["id"])), Some(&body))?;
                println!("updated:");
                print_routine(unwrap(&r, "routine"), unit);
            }
        }
        Cmd::Folders => {
            for f in all_folders(&hevy)? {
                println!("{}\t{}", f["id"], s(&f["title"]));
            }
        }
        Cmd::AddFolder { title } => {
            let r = hevy.call("POST", "/v1/routine_folders", Some(&json!({"routine_folder": {"title": title}})))?;
            let f = unwrap(&r, "routine_folder");
            println!("created folder {} ({})", s(&f["title"]), f["id"]);
        }
        Cmd::History { exercise, since } => {
            let mut res = Resolver::new(&hevy)?;
            let id = res.resolve(&exercise.join(" "))?;
            let title = res.templates.iter().find(|t| t["id"] == id.as_str()).map_or(id.as_str(), |t| s(&t["title"])).to_string();
            let mut path = format!("/v1/exercise_history/{id}");
            if let Some(since) = since {
                let t = stats::parse_since(&since, Utc::now())?;
                path += &format!("?start_date={}", t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            }
            let r = hevy.get(&path)?;
            println!("{title} ({id})");
            print!("{}", stats::history(r["exercise_history"].as_array().map_or(&[][..], |v| v), unit));
        }
        Cmd::Stats { since } => {
            let now = Utc::now();
            let from = stats::parse_since(&since, now)?;
            let ws = hevy.pages("/v1/workouts", "workouts", 10, |page| {
                page.iter().any(|w| w["start_time"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).is_some_and(|t| t >= from))
            })?;
            let templates = hevy.templates(false)?;
            print!(
                "{}",
                stats::summary(&ws, from, now, unit, |id| templates.iter().find(|t| t["id"] == id).and_then(|t| t["primary_muscle_group"].as_str()).map(String::from))
            );
        }
        Cmd::Body { limit } => {
            let mut seen = 0;
            let all = hevy.pages("/v1/body_measurements", "body_measurements", 10, |page| {
                seen += page.len();
                seen < limit
            })?;
            for b in all.iter().take(limit) {
                println!("{}", fmt_measurement(b, unit));
            }
        }
        Cmd::Measure { fields, date } => {
            let date = date.unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
            chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| anyhow!("--date {date:?}: use YYYY-MM-DD"))?;
            let new = measurement_fields(&fields, unit)?;
            let mut post = new.clone();
            post.insert("date".into(), date.clone().into());
            match hevy.call("POST", "/v1/body_measurements", Some(&Value::Object(post))) {
                Ok(_) => {}
                Err(e) if matches!(e.downcast_ref::<HttpError>(), Some(h) if h.code == 409) => {
                    // An entry exists for the date, and PUT nulls every field it isn't
                    // given, so merge into what's there.
                    let existing = hevy.get(&format!("/v1/body_measurements/{date}"))?;
                    let mut merged: Map<String, Value> = existing.as_object().cloned().unwrap_or_default();
                    merged.remove("date");
                    merged.extend(new);
                    hevy.call("PUT", &format!("/v1/body_measurements/{date}"), Some(&Value::Object(merged)))?;
                }
                Err(e) => return Err(e),
            }
            println!("logged {}", fmt_measurement(&hevy.get(&format!("/v1/body_measurements/{date}"))?, unit));
        }
        Cmd::Exercises { query, muscle, custom, refresh } => {
            let q = query.join(" ");
            for t in hevy.templates(refresh)? {
                let title = s(&t["title"]);
                if (q.is_empty() || spec::words_match(&q, title))
                    && muscle.as_deref().is_none_or(|m| t["primary_muscle_group"].as_str().is_some_and(|p| p.eq_ignore_ascii_case(m)))
                    && (!custom || t["is_custom"] == true)
                {
                    println!("{}\t{title}\t{}\t{}\t{}{}", s(&t["id"]), s(&t["type"]), s(&t["equipment"]), s(&t["primary_muscle_group"]), if t["is_custom"] == true { "\tcustom" } else { "" });
                }
            }
        }
        Cmd::AddExercise { title, kind, equipment, muscle, other } => {
            let body = json!({ "exercise": {
                "title": title, "exercise_type": kind, "equipment_category": equipment,
                "muscle_group": muscle, "other_muscles": other,
            }});
            let r = hevy.call("POST", "/v1/exercise_templates", Some(&body))?;
            let _ = std::fs::remove_file(api::cache_path());
            println!("created exercise {title} ({})", r["id"].as_str().map(String::from).unwrap_or_else(|| r["id"].to_string()));
        }
        Cmd::User => pretty(&hevy.get("/v1/user/info")?),
        Cmd::Get { path } => pretty(&hevy.get(&path)?),
        Cmd::Post { path, body: b } => pretty(&hevy.call("POST", &path, Some(&body(&b)?))?),
        Cmd::Put { path, body: b } => pretty(&hevy.call("PUT", &path, Some(&body(&b)?))?),
    }
    Ok(())
}

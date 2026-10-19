//! hevy — the Hevy workout tracker's public API from the terminal.
//!
//! Auth: an API key from https://hevy.com/settings?developer (Hevy Pro), read from
//! HEVY_API_KEY or the file in `[hevy] api_key_file` (default `~/.hevy_api_key`).
//! Exercise templates are cached in ~/.cache/claude-skills/hevy-exercises.json so names in
//! a workout spec resolve without paging through the whole library on every call.

mod spec;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use spec::{Match, Spec, Unit};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime};

const API: &str = "https://api.hevyapp.com";
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Parser)]
#[command(name = "hevy", version, about)]
struct Cli {
    /// Unit for bare weights in specs and for output (kg or lb); default from [hevy] units
    #[arg(long, global = true)]
    units: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
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
    /// One workout with its exercises and sets
    Workout {
        id: String,
        #[arg(long)]
        json: bool,
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

struct Hevy {
    key: String,
}

impl Hevy {
    fn new() -> Result<Self> {
        if let Ok(k) = std::env::var("HEVY_API_KEY")
            && !k.trim().is_empty()
        {
            return Ok(Self { key: k.trim().to_string() });
        }
        let cfg = common::load()?.hevy;
        let path = common::expand(cfg.api_key_file.as_deref().unwrap_or("~/.hevy_api_key"));
        let key = std::fs::read_to_string(&path).map(|s| s.trim().to_string()).map_err(|_| {
            anyhow!("no Hevy API key: put it in {} or set HEVY_API_KEY. Get one (Hevy Pro) at https://hevy.com/settings?developer", path.display())
        })?;
        Ok(Self { key })
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{API}{}", if path.starts_with('/') { path.to_string() } else { format!("/{path}") });
        for attempt in 0..5 {
            let req = ureq::request(method, &url).set("api-key", &self.key).set("Accept", "application/json");
            let res = match body {
                Some(b) => req.send_json(b),
                None => req.call(),
            };
            match res {
                Ok(r) => {
                    let text = r.into_string()?;
                    return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) });
                }
                Err(ureq::Error::Status(429, r)) if attempt < 4 => {
                    sleep(Duration::from_secs(r.header("Retry-After").and_then(|s| s.parse().ok()).unwrap_or(1 + attempt)));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or(text);
                    let hint = match code {
                        401 => " (check the API key; the API needs Hevy Pro)",
                        404 => " (no such id or path)",
                        _ => "",
                    };
                    bail!("HTTP {code}: {}{hint}", msg.trim());
                }
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!()
    }

    /// Every exercise template, from the cache when it is fresh.
    fn templates(&self, refresh: bool) -> Result<Vec<Value>> {
        let path = cache_path();
        let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age < CACHE_TTL);
        if !refresh && fresh
            && let Ok(v) = serde_json::from_str::<Vec<Value>>(&std::fs::read_to_string(&path)?)
        {
            return Ok(v);
        }
        let mut all = Vec::new();
        let mut page = 1;
        loop {
            let r = self.call("GET", &format!("/v1/exercise_templates?page={page}&pageSize=100"), None)?;
            all.extend(r["exercise_templates"].as_array().cloned().unwrap_or_default());
            if page >= r["page_count"].as_u64().unwrap_or(1) {
                break;
            }
            page += 1;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string(&all)?)?;
        Ok(all)
    }
}

fn cache_path() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| common::home().join(".cache"));
    base.join("claude-skills/hevy-exercises.json")
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

/// Number without float noise: 102.05823 -> "102.06", 80.0 -> "80".
fn num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn local(t: &str) -> String {
    DateTime::parse_from_rfc3339(t).map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|_| t.to_string())
}

fn fmt_set(set: &Value, unit: Unit) -> String {
    let mut parts = Vec::new();
    if let Some(kg) = set["weight_kg"].as_f64() {
        parts.push(format!("{} {}", num(unit.kg_to(kg)), unit.name()));
    }
    if let Some(r) = set["reps"].as_f64() {
        parts.push(if parts.is_empty() { format!("{} reps", num(r)) } else { format!("× {}", num(r)) });
    }
    if let Some(m) = set["distance_meters"].as_f64() {
        parts.push(if m >= 1000.0 { format!("{} km", num(m / 1000.0)) } else { format!("{} m", num(m)) });
    }
    if let Some(d) = set["duration_seconds"].as_u64() {
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
    for (i, e) in w["exercises"].as_array().into_iter().flatten().enumerate() {
        let ss = e["superset_id"].as_u64().map(|n| format!("  [superset {n}]")).unwrap_or_default();
        println!("{}. {}{ss}", i + 1, e["title"].as_str().unwrap_or_else(|| s(&e["exercise_template_id"])));
        if let Some(n) = e["notes"].as_str().filter(|n| !n.is_empty()) {
            println!("   {n}");
        }
        for set in e["sets"].as_array().into_iter().flatten() {
            println!("   - {}", fmt_set(set, unit));
        }
    }
}

/// Resolve the spec's exercise names (refetching the library once on a miss, for
/// templates created since the cache was written) and build the request body.
fn build(hevy: &Hevy, arg: &str, unit: Unit) -> Result<(Value, Vec<Value>)> {
    let text = common::read_arg(arg)?;
    let spec: Spec = serde_json::from_str(&text).context("reading the workout spec")?;
    let mut templates = hevy.templates(false)?;
    let mut refreshed = false;
    let body = spec::build(&spec, unit, Utc::now(), |name| loop {
        let pairs = templates.iter().map(|t| (t["id"].as_str().unwrap_or(""), t["title"].as_str().unwrap_or("")));
        match spec::find_template(name, pairs) {
            Match::One(id) => return Ok(id),
            Match::Many(c) => bail!("{name:?} matches several exercises; use the exact title or id:\n  {}", c.join("\n  ")),
            Match::None if !refreshed => {
                templates = hevy.templates(true)?;
                refreshed = true;
            }
            Match::None => bail!("no exercise matches {name:?}; search with `hevy exercises <words>` or create it with `hevy add-exercise`"),
        }
    })?;
    Ok((body, templates))
}

/// The request body with template titles filled in, for a dry-run preview.
fn preview(body: &Value, templates: &[Value]) -> Value {
    let mut w = body["workout"].clone();
    for e in w["exercises"].as_array_mut().into_iter().flatten() {
        if let Some(t) = templates.iter().find(|t| t["id"] == e["exercise_template_id"]) {
            e["title"] = json!(format!("{} ({})", s(&t["title"]), s(&t["id"])));
        }
    }
    w
}

/// POST/PUT answer with `{"workout": [w]}` or `{"workout": w}`.
fn returned_workout(r: &Value) -> &Value {
    match &r["workout"] {
        Value::Array(a) if !a.is_empty() => &a[0],
        Value::Null => r,
        w => w,
    }
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
            let (body, templates) = build(&hevy, &spec, unit)?;
            print_workout(&preview(&body, &templates), unit);
            if json {
                pretty(&body);
            }
            println!("(dry run: nothing sent)");
        }
        Cmd::Create { spec, json, .. } => {
            let (body, _) = build(&hevy, &spec, unit)?;
            if json {
                pretty(&body);
            }
            let r = hevy.call("POST", "/v1/workouts", Some(&body))?;
            println!("created:");
            print_workout(returned_workout(&r), unit);
        }
        Cmd::Update { id, spec, json, .. } => {
            let (body, _) = build(&hevy, &spec, unit)?;
            if json {
                pretty(&body);
            }
            let r = hevy.call("PUT", &format!("/v1/workouts/{id}"), Some(&body))?;
            println!("updated:");
            print_workout(returned_workout(&r), unit);
        }
        Cmd::Workouts { limit } => {
            let mut page = 1;
            let mut shown = 0;
            'pages: loop {
                let r = hevy.call("GET", &format!("/v1/workouts?page={page}&pageSize=10"), None)?;
                for w in r["workouts"].as_array().into_iter().flatten() {
                    if shown == limit {
                        break 'pages;
                    }
                    let n = w["exercises"].as_array().map_or(0, |e| e.len());
                    println!("{}\t{}\t{}\t{n} exercises", s(&w["id"]), local(s(&w["start_time"])), s(&w["title"]));
                    shown += 1;
                }
                if page >= r["page_count"].as_u64().unwrap_or(1) {
                    break;
                }
                page += 1;
            }
        }
        Cmd::Workout { id, json } => {
            let r = hevy.call("GET", &format!("/v1/workouts/{id}"), None)?;
            let w = returned_workout(&r);
            if json { pretty(w) } else { print_workout(w, unit) }
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
            let _ = std::fs::remove_file(cache_path());
            println!("created exercise {title} ({})", r["id"].as_str().map(String::from).unwrap_or_else(|| r["id"].to_string()));
        }
        Cmd::User => pretty(&hevy.call("GET", "/v1/user/info", None)?),
        Cmd::Get { path } => pretty(&hevy.call("GET", &path, None)?),
        Cmd::Post { path, body: b } => pretty(&hevy.call("POST", &path, Some(&body(&b)?))?),
        Cmd::Put { path, body: b } => pretty(&hevy.call("PUT", &path, Some(&body(&b)?))?),
    }
    Ok(())
}

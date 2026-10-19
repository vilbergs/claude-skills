//! The workout spec `hevy create` accepts, and its translation into the API body.
//!
//! A spec is JSON with exercises named by title (or template id) and sets either as
//! objects or as shorthand strings like "3x8@80", "5x5@100kg rpe8", "w 10@40",
//! "3x60s", "5km 30:00". See `parse_shorthand` for the grammar.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

pub const LB: f64 = 0.45359237;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Kg,
    Lb,
}

impl Unit {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "kg" | "kgs" => Ok(Unit::Kg),
            "lb" | "lbs" => Ok(Unit::Lb),
            _ => bail!("unknown unit {s:?}: use kg or lb"),
        }
    }

    pub fn to_kg(self, w: f64) -> f64 {
        match self {
            Unit::Kg => w,
            Unit::Lb => w * LB,
        }
    }

    pub fn kg_to(self, kg: f64) -> f64 {
        match self {
            Unit::Kg => kg,
            Unit::Lb => kg / LB,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Unit::Kg => "kg",
            Unit::Lb => "lb",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// RFC 3339, local "YYYY-MM-DD HH:MM", or "HH:MM" today.
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    /// "1h15m", "75m", "1:15", or a number of minutes.
    #[serde(default)]
    pub duration: Option<Value>,
    #[serde(default)]
    pub private: bool,
    pub exercises: Vec<ExerciseSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExerciseSpec {
    /// Template title or id.
    pub exercise: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// Exercises sharing a number form a superset.
    #[serde(default)]
    pub superset: Option<u32>,
    /// A shorthand string, or a list of shorthand strings and set objects.
    pub sets: Value,
}

/// Build the `{"workout": {...}}` body. `resolve` maps an exercise name to a template id.
pub fn build(spec: &Spec, unit: Unit, now: DateTime<Utc>, mut resolve: impl FnMut(&str) -> Result<String>) -> Result<Value> {
    if spec.exercises.is_empty() {
        bail!("the workout has no exercises");
    }
    let (start, end) = times(spec, now)?;
    let mut exercises = Vec::new();
    for (i, e) in spec.exercises.iter().enumerate() {
        let ctx = || format!("exercise {} ({})", i + 1, e.exercise);
        let id = resolve(&e.exercise).with_context(ctx)?;
        let sets = sets(&e.sets, unit).with_context(ctx)?;
        if sets.is_empty() {
            bail!("{} has no sets", ctx());
        }
        exercises.push(json!({
            "exercise_template_id": id,
            "superset_id": e.superset,
            "notes": e.notes,
            "sets": sets,
        }));
    }
    Ok(json!({ "workout": {
        "title": spec.title,
        "description": spec.description,
        "start_time": rfc3339(start),
        "end_time": rfc3339(end),
        "is_private": spec.private,
        "exercises": exercises,
    }}))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Start and end from whichever of start/end/duration are given; missing end means now,
/// missing duration means an hour.
pub fn times(spec: &Spec, now: DateTime<Utc>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start = spec.start.as_deref().map(|s| parse_time(s, now)).transpose().context("start")?;
    let end = spec.end.as_deref().map(|s| parse_time(s, now)).transpose().context("end")?;
    let dur = spec.duration.as_ref().map(parse_duration).transpose().context("duration")?;
    let (start, end) = match (start, end) {
        (Some(s), Some(e)) => (s, e),
        (Some(s), None) => (s, s + dur.unwrap_or(Duration::hours(1))),
        (None, Some(e)) => (e - dur.unwrap_or(Duration::hours(1)), e),
        (None, None) => (now - dur.unwrap_or(Duration::hours(1)), now),
    };
    if end <= start {
        bail!("the workout ends ({}) before it starts ({})", rfc3339(end), rfc3339(start));
    }
    Ok((start, end))
}

pub fn parse_time(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    let naive = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .or_else(|| {
            let t = NaiveTime::parse_from_str(s, "%H:%M").ok()?;
            Some(now.with_timezone(&Local).date_naive().and_time(t))
        })
        .ok_or_else(|| anyhow!("can't read time {s:?}: use RFC 3339, \"YYYY-MM-DD HH:MM\" or \"HH:MM\""))?;
    Local.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc)).ok_or_else(|| anyhow!("{s:?} doesn't exist in the local time zone"))
}

static DUR_HM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:(\d+)h)?\s*(?:(\d+)(?:m|min))?$").unwrap());

pub fn parse_duration(v: &Value) -> Result<Duration> {
    if let Some(m) = v.as_f64() {
        return Ok(Duration::seconds((m * 60.0).round() as i64));
    }
    let s = v.as_str().ok_or_else(|| anyhow!("duration must be a string or minutes"))?.trim().to_lowercase();
    if let Some((h, m)) = s.split_once(':') {
        return Ok(Duration::hours(h.parse()?) + Duration::minutes(m.parse()?));
    }
    if let Ok(m) = s.parse::<i64>() {
        return Ok(Duration::minutes(m));
    }
    match DUR_HM.captures(&s) {
        Some(c) if !s.is_empty() => {
            let n = |i| c.get(i).map_or(0, |m: regex::Match| m.as_str().parse().unwrap());
            Ok(Duration::hours(n(1)) + Duration::minutes(n(2)))
        }
        _ => bail!("can't read duration {s:?}: use \"1h15m\", \"75m\", \"1:15\" or minutes"),
    }
}

/// Expand a `sets` value into API sets.
pub fn sets(v: &Value, unit: Unit) -> Result<Vec<Value>> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in items {
        match item {
            Value::String(s) => out.extend(parse_shorthand(s, unit)?),
            Value::Object(o) => out.extend(parse_object(o, unit)?),
            other => bail!("a set must be a string or an object, got {other}"),
        }
    }
    Ok(out)
}

fn blank() -> Map<String, Value> {
    let mut m = Map::new();
    for k in ["type", "weight_kg", "reps", "distance_meters", "duration_seconds", "custom_metric", "rpe"] {
        m.insert(k.into(), Value::Null);
    }
    m["type"] = "normal".into();
    m
}

const RPE: [f64; 8] = [6.0, 7.0, 7.5, 8.0, 8.5, 9.0, 9.5, 10.0];

fn rpe(x: f64) -> Result<Value> {
    if !RPE.contains(&x) {
        bail!("RPE {x} not allowed: Hevy takes 6, 7, 7.5, 8, 8.5, 9, 9.5 or 10");
    }
    Ok(x.into())
}

fn set_type(s: &str) -> Option<&'static str> {
    Some(match s {
        "normal" | "n" => "normal",
        "warmup" | "w" => "warmup",
        "failure" | "f" => "failure",
        "dropset" | "d" => "dropset",
        _ => return None,
    })
}

fn repeat(m: Map<String, Value>, count: usize) -> Result<Vec<Value>> {
    if count == 0 {
        bail!("set count is 0");
    }
    Ok(vec![Value::Object(m); count])
}

static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*@\s*").unwrap());
static COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)x(.+)$").unwrap());
static WEIGHT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)(kg|kgs|lb|lbs)?$").unwrap());
static DIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)(km|m|mi)$").unwrap());
static SECS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)(s|sec|min)$").unwrap());
static CLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+):(\d{2})(?::(\d{2}))?$").unwrap());
static RPE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^rpe(\d+(?:\.5)?)$").unwrap());

/// Shorthand for one or more identical sets, tokens separated by spaces:
///
/// - `3x...` (first token) repeats the set 3 times
/// - `8` reps; `@80`, `@80kg`, `@175lb`, `80kg` weight (bare numbers use the default
///   unit); `@bw` no weight
/// - `5km`, `400m`, `2mi` distance; `60s`, `2min`, `1:30`, `1:02:03` duration
/// - `rpe8`, `rpe9.5`; `w`/`warmup`, `f`/`failure`, `d`/`dropset`
pub fn parse_shorthand(s: &str, unit: Unit) -> Result<Vec<Value>> {
    let ctx = || format!("set {s:?}");
    let lower = AT.replace_all(&s.to_lowercase(), " @").into_owned();
    let mut m = blank();
    let mut count = 1;
    let mut tokens: Vec<String> = lower.split_whitespace().map(String::from).collect();
    if tokens.is_empty() {
        bail!("empty set");
    }
    // The repeat prefix sits on the first token after any set type: "3x8@80", "w 2x10@40".
    if let Some(i) = tokens.iter().position(|t| set_type(t).is_none())
        && let Some(c) = COUNT.captures(&tokens[i].clone())
    {
        count = c[1].parse().with_context(ctx)?;
        tokens[i] = c[2].to_string();
    }
    for t in &tokens {
        let t = t.as_str();
        if let Some(ty) = set_type(t) {
            m["type"] = ty.into();
        } else if t == "@bw" || t == "bw" {
            m["weight_kg"] = Value::Null;
        } else if let Some(w) = t.strip_prefix('@') {
            let c = WEIGHT.captures(w).ok_or_else(|| anyhow!("can't read weight {w:?}")).with_context(ctx)?;
            m["weight_kg"] = weight(&c, unit).into();
        } else if t.chars().all(|c| c.is_ascii_digit()) {
            if !m["reps"].is_null() {
                bail!("{}: reps given twice", ctx());
            }
            m["reps"] = t.parse::<u64>()?.into();
        } else if let Some(c) = WEIGHT.captures(t).filter(|c| c.get(2).is_some()) {
            m["weight_kg"] = weight(&c, unit).into();
        } else if let Some(c) = DIST.captures(t) {
            let x: f64 = c[1].parse()?;
            let meters = match &c[2] {
                "km" => x * 1000.0,
                "mi" => x * 1609.344,
                _ => x,
            };
            m["distance_meters"] = (meters.round() as u64).into();
        } else if let Some(c) = SECS.captures(t) {
            let x: u64 = c[1].parse()?;
            m["duration_seconds"] = (if &c[2] == "min" { x * 60 } else { x }).into();
        } else if let Some(c) = CLOCK.captures(t) {
            let a: u64 = c[1].parse()?;
            let b: u64 = c[2].parse()?;
            m["duration_seconds"] = match c.get(3) {
                Some(s) => a * 3600 + b * 60 + s.as_str().parse::<u64>()?,
                None => a * 60 + b,
            }
            .into();
        } else if let Some(c) = RPE_RE.captures(t) {
            m["rpe"] = rpe(c[1].parse()?).with_context(ctx)?;
        } else {
            bail!("{}: don't understand {t:?}", ctx());
        }
    }
    repeat(m, count)
}

fn weight(c: &regex::Captures, unit: Unit) -> f64 {
    let x: f64 = c[1].parse().unwrap();
    match c.get(2).map(|m| m.as_str()) {
        Some("kg" | "kgs") => x,
        Some(_) => Unit::Lb.to_kg(x),
        None => unit.to_kg(x),
    }
}

/// A set object: API field names (weight_kg, reps, distance_meters, duration_seconds,
/// custom_metric, rpe, type) plus conveniences: kg, lb, weight (default unit), km, m, mi,
/// seconds, duration ("1:30"), count.
fn parse_object(o: &Map<String, Value>, unit: Unit) -> Result<Vec<Value>> {
    let ctx = || format!("set {}", Value::Object(o.clone()));
    let num = |v: &Value| v.as_f64().ok_or_else(|| anyhow!("{v} is not a number"));
    let mut m = blank();
    let mut count = 1;
    for (k, v) in o {
        let r: Result<()> = (|| {
            match k.as_str() {
                "type" => m["type"] = set_type(v.as_str().unwrap_or("")).ok_or_else(|| anyhow!("unknown set type {v}"))?.into(),
                "count" => count = v.as_u64().ok_or_else(|| anyhow!("count must be a whole number"))? as usize,
                "reps" => m["reps"] = v.clone(),
                "weight_kg" | "kg" => m["weight_kg"] = v.clone(),
                "lb" | "lbs" => m["weight_kg"] = Unit::Lb.to_kg(num(v)?).into(),
                "weight" => m["weight_kg"] = unit.to_kg(num(v)?).into(),
                "distance_meters" | "m" => m["distance_meters"] = v.clone(),
                "km" => m["distance_meters"] = ((num(v)? * 1000.0).round() as u64).into(),
                "mi" => m["distance_meters"] = ((num(v)? * 1609.344).round() as u64).into(),
                "duration_seconds" | "seconds" => m["duration_seconds"] = v.clone(),
                "duration" => {
                    let set = parse_shorthand(v.as_str().ok_or_else(|| anyhow!("duration must be a string like \"1:30\" or \"90s\""))?, unit)?;
                    m["duration_seconds"] = set[0]["duration_seconds"].clone();
                }
                "custom_metric" => m["custom_metric"] = v.clone(),
                "rpe" => m["rpe"] = if v.is_null() { Value::Null } else { rpe(num(v)?)? },
                _ => bail!("unknown field {k:?}"),
            }
            Ok(())
        })();
        r.with_context(ctx)?;
    }
    repeat(m, count)
}

/// Lowercase, punctuation to spaces, collapsed: "Bench Press (Barbell)" -> "bench press barbell".
fn normalize(s: &str) -> String {
    s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every word of the query starts a word of the title: "inc bench" matches
/// "Incline Bench Press (Barbell)".
pub fn words_match(query: &str, title: &str) -> bool {
    let nt = normalize(title);
    let tw: Vec<&str> = nt.split(' ').collect();
    normalize(query).split(' ').filter(|w| !w.is_empty()).all(|w| tw.iter().any(|x| x.starts_with(w)))
}

pub enum Match {
    One(String),
    None,
    Many(Vec<String>),
}

/// Find a template among (id, title) pairs: by id, by title ignoring case and punctuation,
/// then by every query word appearing in the title.
pub fn find_template<'a>(query: &str, templates: impl IntoIterator<Item = (&'a str, &'a str)> + Clone) -> Match {
    let q = query.trim();
    if let Some((id, _)) = templates.clone().into_iter().find(|(id, _)| id.eq_ignore_ascii_case(q)) {
        return Match::One(id.to_string());
    }
    let nq = normalize(q);
    let exact: Vec<_> = templates.clone().into_iter().filter(|(_, t)| normalize(t) == nq).collect();
    if let [(id, _)] = exact.as_slice() {
        return Match::One(id.to_string());
    }
    if nq.is_empty() {
        return Match::None;
    }
    let hits: Vec<_> = templates.into_iter().filter(|(_, t)| words_match(&nq, t)).collect();
    match hits.as_slice() {
        [] => Match::None,
        [(id, _)] => Match::One(id.to_string()),
        many => Match::Many(many.iter().map(|(id, t)| format!("{t} ({id})")).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Value {
        let v = parse_shorthand(s, Unit::Kg).unwrap();
        assert_eq!(v.len(), 1, "{s}");
        v[0].clone()
    }

    #[test]
    fn shorthand_weight_reps() {
        let v = parse_shorthand("3x8@80", Unit::Kg).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0]["reps"], 8);
        assert_eq!(v[0]["weight_kg"], 80.0);
        assert_eq!(v[0]["type"], "normal");
        let s = one("w 10 @ 40kg");
        assert_eq!((s["type"].as_str(), s["reps"].as_u64(), s["weight_kg"].as_f64()), (Some("warmup"), Some(10), Some(40.0)));
        assert_eq!(one("5@100kg rpe8.5")["rpe"], 8.5);
        assert_eq!(one("12@bw")["weight_kg"], Value::Null);
        assert_eq!(parse_shorthand("f 2x6@60", Unit::Kg).unwrap().len(), 2);
    }

    #[test]
    fn shorthand_units() {
        let lb = parse_shorthand("5@100", Unit::Lb).unwrap();
        assert!((lb[0]["weight_kg"].as_f64().unwrap() - 45.359237).abs() < 1e-9);
        assert!((one("5@225lb")["weight_kg"].as_f64().unwrap() - 102.0582).abs() < 1e-3);
        assert_eq!(parse_shorthand("5@100kg", Unit::Lb).unwrap()[0]["weight_kg"], 100.0);
    }

    #[test]
    fn shorthand_cardio() {
        assert_eq!(one("5km 30:00")["distance_meters"], 5000);
        assert_eq!(one("5km 30:00")["duration_seconds"], 1800);
        assert_eq!(one("1:02:03")["duration_seconds"], 3723);
        assert_eq!(parse_shorthand("3x60s", Unit::Kg).unwrap()[2]["duration_seconds"], 60);
        assert_eq!(one("2min")["duration_seconds"], 120);
        assert_eq!(one("400m")["distance_meters"], 400);
    }

    #[test]
    fn shorthand_errors() {
        assert!(parse_shorthand("8 8", Unit::Kg).is_err());
        assert!(parse_shorthand("8@heavy", Unit::Kg).is_err());
        assert!(parse_shorthand("5 rpe5", Unit::Kg).is_err());
        assert!(parse_shorthand("0x5", Unit::Kg).is_err());
        assert!(parse_shorthand("", Unit::Kg).is_err());
    }

    #[test]
    fn objects() {
        let v = sets(&json!([{"reps": 5, "lb": 135, "count": 2}, {"duration": "1:30"}, "8@60"]), Unit::Kg).unwrap();
        assert_eq!(v.len(), 4);
        assert!((v[0]["weight_kg"].as_f64().unwrap() - 61.235).abs() < 1e-3);
        assert_eq!(v[2]["duration_seconds"], 90);
        assert!(sets(&json!([{"repz": 5}]), Unit::Kg).is_err());
        assert!(sets(&json!([{"rpe": 11}]), Unit::Kg).is_err());
    }

    fn spec(start: Option<&str>, end: Option<&str>, duration: Option<Value>) -> Spec {
        Spec { title: "t".into(), description: None, start: start.map(Into::into), end: end.map(Into::into), duration, private: false, exercises: vec![] }
    }

    #[test]
    fn time_resolution() {
        let now = DateTime::parse_from_rfc3339("2026-10-19T18:00:00Z").unwrap().with_timezone(&Utc);
        let fmt = |(a, b): (DateTime<Utc>, DateTime<Utc>)| (rfc3339(a), rfc3339(b));
        assert_eq!(fmt(times(&spec(None, None, None), now).unwrap()), ("2026-10-19T17:00:00Z".into(), "2026-10-19T18:00:00Z".into()));
        assert_eq!(fmt(times(&spec(Some("2026-10-19T08:00:00+02:00"), None, Some(json!("1h15m"))), now).unwrap()).1, "2026-10-19T07:15:00Z");
        assert_eq!(fmt(times(&spec(None, Some("2026-10-19T10:00:00Z"), Some(json!(45))), now).unwrap()).0, "2026-10-19T09:15:00Z");
        assert!(times(&spec(Some("2026-10-19T10:00:00Z"), Some("2026-10-19T09:00:00Z"), None), now).is_err());
    }

    #[test]
    fn durations() {
        let mins = |v: Value| parse_duration(&v).unwrap().num_minutes();
        assert_eq!(mins(json!("1h15m")), 75);
        assert_eq!(mins(json!("75m")), 75);
        assert_eq!(mins(json!("45min")), 45);
        assert_eq!(mins(json!("2h")), 120);
        assert_eq!(mins(json!("1:15")), 75);
        assert_eq!(mins(json!(50)), 50);
        assert!(parse_duration(&json!("soon")).is_err());
        assert!(parse_duration(&json!("")).is_err());
    }

    #[test]
    fn template_matching() {
        let t = [
            ("79D0BB3A", "Bench Press (Barbell)"),
            ("3601968B", "Bench Press (Dumbbell)"),
            ("50DFDFAB", "Incline Bench Press (Barbell)"),
            ("D04AC939", "Squat (Barbell)"),
        ];
        let id = |q| match find_template(q, t.iter().map(|(a, b)| (*a, *b))) {
            Match::One(id) => Some(id),
            _ => None,
        };
        assert_eq!(id("bench press (barbell)").as_deref(), Some("79D0BB3A"));
        assert_eq!(id("Bench Press Barbell").as_deref(), Some("79D0BB3A"));
        assert_eq!(id("d04ac939").as_deref(), Some("D04AC939"));
        assert_eq!(id("squat").as_deref(), Some("D04AC939"));
        assert_eq!(id("incline bench").as_deref(), Some("50DFDFAB"));
        assert!(matches!(find_template("bench", t.iter().map(|(a, b)| (*a, *b))), Match::Many(v) if v.len() == 3));
        assert!(matches!(find_template("deadlift", t.iter().map(|(a, b)| (*a, *b))), Match::None));
    }

    #[test]
    fn build_body() {
        let s: Spec = serde_json::from_value(json!({
            "title": "Legs", "start": "2026-10-19T17:00:00Z", "duration": "1h",
            "exercises": [{"exercise": "squat", "sets": ["w 5@60", "3x5@100"], "notes": "deep", "superset": 0}]
        }))
        .unwrap();
        let now = Utc::now();
        let b = build(&s, Unit::Kg, now, |_| Ok("D04AC939".into())).unwrap();
        let w = &b["workout"];
        assert_eq!(w["end_time"], "2026-10-19T18:00:00Z");
        assert_eq!(w["is_private"], false);
        assert_eq!(w["exercises"][0]["exercise_template_id"], "D04AC939");
        assert_eq!(w["exercises"][0]["superset_id"], 0);
        assert_eq!(w["exercises"][0]["sets"].as_array().unwrap().len(), 4);
        assert!(serde_json::from_value::<Spec>(json!({"title": "x", "exercises": [], "when": "now"})).is_err());
    }
}

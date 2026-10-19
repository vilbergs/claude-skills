//! Progress numbers: per-exercise history (best sets, estimated 1RM, trend) and training
//! summaries over a period. Warm-up sets never count.

use crate::spec::{Unit, num};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

/// Epley estimate; a single is its own 1RM.
pub fn e1rm(kg: f64, reps: f64) -> f64 {
    if reps <= 1.0 { kg } else { kg * (1.0 + reps / 30.0) }
}

/// "30d", "12w", "6mo", "1y" before now, or a date.
pub fn parse_since(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = s.trim().to_lowercase();
    if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().map(|t| t.with_timezone(&Utc)).ok_or_else(|| anyhow!("bad date {s}"));
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().map_err(|_| anyhow!("can't read {s:?}: use 30d, 12w, 6mo, 1y or YYYY-MM-DD"))?;
    let days = match unit {
        "d" => n,
        "w" => n * 7,
        "mo" => n * 30,
        "y" => n * 365,
        _ => return Err(anyhow!("can't read {s:?}: use 30d, 12w, 6mo, 1y or YYYY-MM-DD")),
    };
    Ok(now - Duration::days(days))
}

fn working(set: &Value, type_key: &str) -> bool {
    set[type_key].as_str() != Some("warmup")
}

fn date(t: &Value) -> String {
    t.as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into())
}

/// How good a set is, for picking a session's best: e1RM when weighted, else reps,
/// duration, distance.
fn score(s: &Value) -> f64 {
    match (s["weight_kg"].as_f64(), s["reps"].as_f64()) {
        (Some(w), Some(r)) => e1rm(w, r),
        (Some(w), None) => w,
        (None, Some(r)) => r,
        _ => s["duration_seconds"].as_f64().or(s["distance_meters"].as_f64()).unwrap_or(0.0),
    }
}

fn fmt_set(s: &Value, unit: Unit) -> String {
    let mut parts = Vec::new();
    match (s["weight_kg"].as_f64(), s["reps"].as_f64()) {
        (Some(w), Some(r)) => parts.push(format!("{} {} × {}", num(unit.kg_to(w)), unit.name(), num(r))),
        (Some(w), None) => parts.push(format!("{} {}", num(unit.kg_to(w)), unit.name())),
        (None, Some(r)) => parts.push(format!("{} reps", num(r))),
        _ => {}
    }
    if let Some(m) = s["distance_meters"].as_f64() {
        parts.push(format!("{} km", num(m / 1000.0)));
    }
    if let Some(d) = s["duration_seconds"].as_u64() {
        parts.push(format!("{}:{:02}", d / 60, d % 60));
    }
    if parts.is_empty() { "-".into() } else { parts.join(" ") }
}

/// The set with the largest value of `key`, among sets that have one; ties go to the
/// better set overall (85 kg × 5 over 85 kg × 4).
fn max_by_field<'a>(sets: &[&'a Value], key: &str) -> Option<&'a Value> {
    sets.iter().copied().filter_map(|s| Some((s[key].as_f64()?, s))).max_by(|a, b| a.0.total_cmp(&b.0).then(score(a.1).total_cmp(&score(b.1)))).map(|(_, s)| s)
}

/// Report on `/v1/exercise_history` entries (one per set), oldest session first.
pub fn history(entries: &[Value], unit: Unit) -> String {
    let mut sessions: Vec<(String, String, String, Vec<&Value>)> = Vec::new();
    let mut sorted: Vec<&Value> = entries.iter().filter(|s| working(s, "set_type")).collect();
    sorted.sort_by(|a, b| a["workout_start_time"].as_str().cmp(&b["workout_start_time"].as_str()));
    for s in sorted {
        let id = s["workout_id"].as_str().unwrap_or("").to_string();
        match sessions.last_mut() {
            Some((last, _, _, sets)) if *last == id => sets.push(s),
            _ => sessions.push((id, date(&s["workout_start_time"]), s["workout_title"].as_str().unwrap_or("-").to_string(), vec![s])),
        }
    }
    if sessions.is_empty() {
        return "no working sets in this period".into();
    }
    let weighted = sessions.iter().flat_map(|s| &s.3).any(|s| s["weight_kg"].as_f64().is_some() && s["reps"].as_f64().is_some());
    let u = unit.name();
    let mut out = if weighted { format!("date\tworkout\tsets\tbest set\te1RM {u}\tvolume {u}\n") } else { "date\tworkout\tsets\tbest set\treps\n".into() };
    let mut e1rms = Vec::new();
    for (_, d, title, sets) in &sessions {
        let best = sets.iter().copied().max_by(|a, b| score(a).total_cmp(&score(b))).unwrap();
        if weighted {
            let vol: f64 = sets.iter().filter_map(|s| Some(s["weight_kg"].as_f64()? * s["reps"].as_f64()?)).sum();
            let e = match (best["weight_kg"].as_f64(), best["reps"].as_f64()) {
                (Some(w), Some(r)) => Some(e1rm(w, r)),
                _ => None,
            };
            if let Some(e) = e {
                e1rms.push((d.clone(), e, best));
            }
            out += &format!("{d}\t{title}\t{}\t{}\t{}\t{}\n", sets.len(), fmt_set(best, unit), e.map_or("-".into(), |e| num(unit.kg_to(e))), num(unit.kg_to(vol)));
        } else {
            let reps: f64 = sets.iter().filter_map(|s| s["reps"].as_f64()).sum();
            out += &format!("{d}\t{title}\t{}\t{}\t{}\n", sets.len(), fmt_set(best, unit), num(reps));
        }
    }
    let all: Vec<&Value> = sessions.iter().flat_map(|s| s.3.iter().copied()).collect();
    let when = |s: &Value| date(&s["workout_start_time"]);
    if let Some((d, e, s)) = e1rms.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
        out += &format!("best e1RM: {} {u} ({}, {d})\n", num(unit.kg_to(*e)), fmt_set(s, unit));
    }
    if let Some(s) = max_by_field(&all, "weight_kg") {
        out += &format!("heaviest: {} ({})\n", fmt_set(s, unit), when(s));
    }
    if let Some(s) = max_by_field(&all, "reps") {
        out += &format!("most reps: {} ({})\n", fmt_set(s, unit), when(s));
    }
    if let Some(s) = max_by_field(&all, "duration_seconds") {
        out += &format!("longest: {} ({})\n", fmt_set(s, unit), when(s));
    }
    if let (Some(first), Some(last)) = (e1rms.first(), e1rms.last())
        && e1rms.len() > 1
    {
        out += &format!(
            "e1RM trend: {} -> {} {u} ({:+.1}%) over {} sessions\n",
            num(unit.kg_to(first.1)),
            num(unit.kg_to(last.1)),
            (last.1 / first.1 - 1.0) * 100.0,
            e1rms.len()
        );
    }
    out
}

/// Summary of workouts since `since`. `muscle` maps a template id to its primary muscle.
pub fn summary(workouts: &[Value], since: DateTime<Utc>, now: DateTime<Utc>, unit: Unit, muscle: impl Fn(&str) -> Option<String>) -> String {
    let mut minutes = 0;
    let mut sets = 0;
    let mut volume = 0.0;
    let mut by_muscle: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_exercise: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_week: BTreeMap<String, usize> = BTreeMap::new();
    let mut n = 0;
    for w in workouts {
        let (Some(a), Some(b)) = (w["start_time"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()), w["end_time"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok())) else {
            continue;
        };
        if a < since {
            continue;
        }
        n += 1;
        minutes += (b - a).num_minutes();
        let wk = a.with_timezone(&Local).iso_week();
        *by_week.entry(format!("{}-W{:02}", wk.year(), wk.week())).or_default() += 1;
        for e in w["exercises"].as_array().into_iter().flatten() {
            let work: Vec<&Value> = e["sets"].as_array().into_iter().flatten().filter(|s| working(s, "type")).collect();
            if work.is_empty() {
                continue;
            }
            sets += work.len();
            volume += work.iter().filter_map(|s| Some(s["weight_kg"].as_f64()? * s["reps"].as_f64()?)).sum::<f64>();
            *by_exercise.entry(e["title"].as_str().unwrap_or("-").to_string()).or_default() += work.len();
            let m = muscle(e["exercise_template_id"].as_str().unwrap_or("")).unwrap_or_else(|| "unknown".into());
            *by_muscle.entry(m).or_default() += work.len();
        }
    }
    let days = (now - since).num_days().max(1) as f64;
    let mut out = format!(
        "{} – {}: {n} workouts ({:.1}/week), {}h {:02}m, {sets} working sets, {} {} volume\n",
        since.with_timezone(&Local).format("%Y-%m-%d"),
        now.with_timezone(&Local).format("%Y-%m-%d"),
        n as f64 / days * 7.0,
        minutes / 60,
        minutes % 60,
        num(unit.kg_to(volume).round()),
        unit.name()
    );
    if n == 0 {
        return out;
    }
    let ranked = |m: &BTreeMap<String, usize>, limit: usize| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.into_iter().take(limit).map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
    };
    out += &format!("working sets by primary muscle: {}\n", ranked(&by_muscle, usize::MAX));
    out += &format!("top exercises (sets): {}\n", ranked(&by_exercise, 8));
    out += &format!("workouts per week: {}\n", by_week.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn epley() {
        assert_eq!(e1rm(100.0, 1.0), 100.0);
        assert!((e1rm(100.0, 10.0) - 133.333).abs() < 1e-2);
    }

    #[test]
    fn since() {
        let now = utc("2026-10-19T12:00:00Z");
        assert_eq!(parse_since("30d", now).unwrap(), utc("2026-09-19T12:00:00Z"));
        assert_eq!(parse_since("2w", now).unwrap(), utc("2026-10-05T12:00:00Z"));
        assert!(parse_since("soon", now).is_err());
        assert!(parse_since("3m", now).is_err());
    }

    fn entry(w: &str, t: &str, kg: f64, reps: u64, ty: &str) -> Value {
        json!({"workout_id": w, "workout_title": "Push", "workout_start_time": t, "weight_kg": kg, "reps": reps, "set_type": ty})
    }

    #[test]
    fn history_report() {
        let e = vec![
            entry("b", "2026-10-12T17:00:00Z", 85.0, 5, "normal"),
            entry("a", "2026-10-05T17:00:00Z", 40.0, 10, "warmup"),
            entry("a", "2026-10-05T17:00:00Z", 80.0, 5, "normal"),
            entry("a", "2026-10-05T17:00:00Z", 80.0, 6, "normal"),
            entry("b", "2026-10-12T17:00:00Z", 85.0, 4, "normal"),
        ];
        let r = history(&e, Unit::Kg);
        let lines: Vec<&str> = r.lines().collect();
        assert!(lines[1].contains("\t2\t80 kg × 6\t96\t880"), "{r}");
        assert!(lines[2].contains("\t2\t85 kg × 5\t99.17\t765"), "{r}");
        assert!(r.contains("best e1RM: 99.17 kg (85 kg × 5"), "{r}");
        assert!(r.contains("heaviest: 85 kg × 5"), "{r}");
        assert!(r.contains("e1RM trend: 96 -> 99.17 kg (+3.3%) over 2 sessions"), "{r}");
        assert!(!r.contains("40 kg"), "warm-ups excluded: {r}");
    }

    #[test]
    fn bodyweight_history() {
        let e = vec![json!({"workout_id": "a", "workout_title": "Pull", "workout_start_time": "2026-10-05T17:00:00Z", "reps": 10, "set_type": "normal"})];
        let r = history(&e, Unit::Kg);
        assert!(r.starts_with("date\tworkout\tsets\tbest set\treps\n"), "{r}");
        assert!(r.contains("most reps: 10 reps"), "{r}");
        assert!(!r.contains("e1RM"), "{r}");
    }

    #[test]
    fn period_summary() {
        let w = |start: &str, end: &str| {
            json!({"title": "Push", "start_time": start, "end_time": end, "exercises": [
                {"title": "Bench Press (Barbell)", "exercise_template_id": "B", "sets": [
                    {"type": "warmup", "weight_kg": 40, "reps": 10}, {"type": "normal", "weight_kg": 80, "reps": 5}, {"type": "normal", "weight_kg": 80, "reps": 5}]},
                {"title": "Pull Up", "exercise_template_id": "P", "sets": [{"type": "normal", "reps": 10}]}]})
        };
        let ws = vec![w("2026-10-12T17:00:00Z", "2026-10-12T18:00:00Z"), w("2026-10-14T17:00:00Z", "2026-10-14T18:30:00Z"), w("2026-08-01T17:00:00Z", "2026-08-01T18:00:00Z")];
        let now = utc("2026-10-19T12:00:00Z");
        let r = summary(&ws, parse_since("14d", now).unwrap(), now, Unit::Kg, |id| Some(if id == "B" { "chest" } else { "lats" }.into()));
        assert!(r.contains(": 2 workouts (1.0/week), 2h 30m, 6 working sets, 1600 kg volume"), "{r}");
        assert!(r.contains("working sets by primary muscle: chest 4, lats 2"), "{r}");
        assert!(r.contains("top exercises (sets): Bench Press (Barbell) 4, Pull Up 2"), "{r}");
    }
}

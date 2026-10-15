//! things — read and write Things 3 on this Mac.
//!
//! Reads go straight to the Things SQLite database (read-only). Writes go through the
//! official things:/// URL scheme (auth token read from the database), and deletes
//! through AppleScript.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Local, NaiveDate};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{Connection, OpenFlags, Row, params_from_iter};
use serde_json::{Map, Value, json};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DB_DIR: &str = "Library/Group Containers/JLMPQHK86H.com.culturedcode.ThingsMac";

#[derive(Parser)]
#[command(name = "things", version, about)]
struct Cli {
    #[arg(long, value_enum, default_value = "text", global = true)]
    format: Format,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Format {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inbox
    Inbox(Limit),
    /// Today, including overdue start dates and deadlines due
    Today(Limit),
    /// Scheduled for a later date
    Upcoming(Limit),
    Anytime(Limit),
    Someday(Limit),
    /// Recently completed/canceled (default 50)
    Logbook(Limit),
    /// Open items with deadlines, soonest first
    Deadlines(Limit),
    /// Repeating templates
    Repeating(Limit),
    /// Open projects
    Projects {
        #[arg(long)]
        area: Option<String>,
    },
    Areas,
    Tags,
    /// Search title and notes
    Search {
        text: String,
        /// Include completed/canceled
        #[arg(long)]
        all: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Open items with a tag
    Tagged { tag: String },
    /// Details of a to-do/project (notes, checklist, items) or an area by name/id
    Show { id: String },
    /// Open the item in the Things app
    Reveal { id: String },
    /// Create a to-do
    Add(AddArgs),
    /// Create a project
    AddProject(AddProjectArgs),
    /// Change a to-do or project
    Update(UpdateArgs),
    Complete { id: String },
    Cancel { id: String },
    /// Move to Trash
    Delete { id: String },
}

#[derive(Args)]
struct Limit {
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Args, Default)]
struct WriteArgs {
    #[arg(long)]
    notes: Option<String>,
    /// today | tomorrow | evening | anytime | someday | YYYY-MM-DD[@HH:MM]
    #[arg(long)]
    when: Option<String>,
    /// YYYY-MM-DD (empty string clears)
    #[arg(long)]
    deadline: Option<String>,
    /// Comma-separated, replaces existing (tags must already exist)
    #[arg(long)]
    tags: Option<String>,
}

#[derive(Args)]
struct AddArgs {
    title: String,
    #[command(flatten)]
    w: WriteArgs,
    /// Project or area name
    #[arg(long)]
    list: Option<String>,
    /// Project or area id
    #[arg(long)]
    list_id: Option<String>,
    /// Heading inside the project
    #[arg(long)]
    heading: Option<String>,
    /// Repeatable
    #[arg(long)]
    checklist: Vec<String>,
}

#[derive(Args)]
struct AddProjectArgs {
    title: String,
    #[command(flatten)]
    w: WriteArgs,
    #[arg(long)]
    area: Option<String>,
    #[arg(long)]
    area_id: Option<String>,
    /// Repeatable: to-do titles inside the project
    #[arg(long)]
    todo: Vec<String>,
}

#[derive(Args, Default)]
struct UpdateArgs {
    id: String,
    #[command(flatten)]
    w: WriteArgs,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    prepend_notes: Option<String>,
    #[arg(long)]
    append_notes: Option<String>,
    #[arg(long)]
    add_tags: Option<String>,
    /// Move to project/area by name
    #[arg(long)]
    list: Option<String>,
    /// Move to project/area by id
    #[arg(long)]
    list_id: Option<String>,
    #[arg(long)]
    heading: Option<String>,
    /// Replace checklist (repeatable)
    #[arg(long)]
    checklist: Vec<String>,
    #[arg(long)]
    append_checklist: Vec<String>,
    #[arg(long, group = "state")]
    complete: bool,
    #[arg(long, group = "state")]
    cancel: bool,
    #[arg(long, group = "state")]
    reopen: bool,
}

// ---------- helpers ----------

fn db() -> Result<Connection> {
    let dir = common::home().join(DB_DIR);
    let path = std::fs::read_dir(&dir)
        .with_context(|| format!("Things database not found under {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ThingsData-")))
        .map(|p| p.join("Things Database.thingsdatabase/main.sqlite"))
        .filter(|p| p.exists())
        .context("Things database not found")?;
    Ok(Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)?)
}

/// Things packs dates as year<<16 | month<<12 | day<<7.
fn enc_date(d: NaiveDate) -> i64 {
    (d.year() as i64) << 16 | (d.month() as i64) << 12 | (d.day() as i64) << 7
}

fn dec_date(v: Option<i64>) -> Option<String> {
    let v = v.filter(|v| *v != 0)?;
    NaiveDate::from_ymd_opt((v >> 16) as i32, ((v >> 12) & 0xF) as u32, ((v >> 7) & 0x1F) as u32).map(|d| d.to_string())
}

fn ts(v: Option<f64>) -> Option<String> {
    let v = v.filter(|v| *v != 0.0)?;
    DateTime::from_timestamp(v as i64, 0).map(|t| t.with_timezone(&Local).format("%Y-%m-%dT%H:%M").to_string())
}

fn today() -> i64 {
    enc_date(Local::now().date_naive())
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

const BASE: &str = r#"
SELECT t.*, p.title AS project_title, a.title AS area_title, pa.title AS project_area_title,
       h.title AS heading_title,
       (SELECT group_concat(tg.title, ', ') FROM TMTaskTag tt JOIN TMTag tg ON tg.uuid = tt.tags
         WHERE tt.tasks = t.uuid) AS tags
FROM TMTask t
LEFT JOIN TMTask h ON h.uuid = t.heading
LEFT JOIN TMTask p ON p.uuid = COALESCE(t.project, h.project)
LEFT JOIN TMArea a ON a.uuid = t.area
LEFT JOIN TMArea pa ON pa.uuid = p.area
WHERE t.trashed = 0
"#;
const OPEN_PROJECT: &str = " AND (p.uuid IS NULL OR (p.trashed = 0 AND p.status = 0))";

fn get<T: rusqlite::types::FromSql>(r: &Row, col: &str) -> Option<T> {
    r.get::<_, Option<T>>(col).ok().flatten()
}

fn row_dict(r: &Row, full: bool) -> Map<String, Value> {
    let kind = get::<i64>(r, "type").unwrap_or(0);
    let status = get::<i64>(r, "status").unwrap_or(0);
    let start = get::<i64>(r, "start").unwrap_or(0);
    let repeating = !matches!(r.get_ref("rt1_recurrenceRule"), Ok(ValueRef::Null) | Err(_));
    let mut d = Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v.filter(|v| !v.is_null() && v != "") {
            d.insert(k.into(), v);
        }
    };
    put("id", get::<String>(r, "uuid").map(Value::from));
    put("type", Some(json!(match kind { 0 => "todo", 1 => "project", 2 => "heading", _ => "?" })));
    put("title", get::<String>(r, "title").map(Value::from));
    put("status", Some(json!(match status { 0 => "open", 2 => "canceled", 3 => "completed", _ => "?" })));
    put("list", Some(json!(match start { 0 => "inbox", 1 => "anytime", 2 => "someday", _ => "?" })));
    put("when", dec_date(get(r, "startDate")).map(Value::from));
    put("evening", (get::<i64>(r, "startBucket") == Some(1)).then_some(json!(true)));
    put("deadline", dec_date(get(r, "deadline")).map(Value::from));
    put("project", get::<String>(r, "project_title").map(Value::from));
    put("heading", get::<String>(r, "heading_title").map(Value::from));
    put("area", get::<String>(r, "area_title").or_else(|| get(r, "project_area_title")).map(Value::from));
    put("tags", get::<String>(r, "tags").map(Value::from));
    put("repeating", repeating.then_some(json!(true)));
    if status != 0 {
        put("done", ts(get(r, "stopDate")).map(Value::from));
    }
    if full {
        put("notes", get::<String>(r, "notes").map(Value::from));
        put("created", ts(get(r, "creationDate")).map(Value::from));
        put("modified", ts(get(r, "userModificationDate")).map(Value::from));
    }
    d
}

fn query(con: &Connection, filter: &str, params: Vec<Sql>, order: &str, limit: Option<usize>) -> Result<Vec<Value>> {
    let mut sql = format!("{BASE}{OPEN_PROJECT} AND {filter} ORDER BY {order}");
    if let Some(l) = limit {
        sql += &format!(" LIMIT {l}");
    }
    let mut st = con.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(params), |r| Ok(Value::Object(row_dict(r, false))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn out(data: Value, fmt: Format) {
    if fmt == Format::Json {
        println!("{}", serde_json::to_string_pretty(&data).unwrap());
        return;
    }
    let items = match data {
        Value::Array(a) => a,
        v => vec![v],
    };
    if items.is_empty() {
        println!("(nothing)");
    }
    for d in items {
        let extra: Vec<String> = ["when", "deadline", "project", "area", "tags", "done"].iter()
            .filter_map(|k| d[*k].as_str().map(|v| format!("{k}={v}"))).collect();
        let mark = if d["type"] == "area" { "▸" } else {
            match d["status"].as_str() { Some("completed") => "[x]", Some("canceled") => "[-]", _ => "[ ]" }
        };
        let extra = if extra.is_empty() { String::new() } else { format!("  · {}", extra.join(" · ")) };
        println!("{mark} {}  ({}){extra}", d["title"].as_str().unwrap_or(""), d["id"].as_str().unwrap_or(""));
        if let Some(n) = d["notes"].as_str() {
            println!("    notes: {}", n.replace('\n', "\n           "));
        }
        for c in d["checklist"].as_array().into_iter().flatten() {
            println!("    {} {}", if c["done"] == true { "[x]" } else { "[ ]" }, c["title"].as_str().unwrap_or(""));
        }
        for c in d["items"].as_array().into_iter().flatten() {
            println!("    - {}  ({})", c["title"].as_str().unwrap_or(""), c["id"].as_str().unwrap_or(""));
        }
    }
}

// ---------- reads ----------

const OPEN_TODO: &str = "t.type = 0 AND t.status = 0 AND t.rt1_recurrenceRule IS NULL";

fn list(con: &Connection, name: &str, limit: Option<usize>) -> Result<Value> {
    let t = today();
    let idx = "t.\"index\"";
    let mut r = match name {
        "inbox" => query(con, &format!("{OPEN_TODO} AND t.start = 0"), vec![], idx, None)?,
        "today" => query(con, &format!("{OPEN_TODO} AND ((t.start = 1 AND t.startDate IS NOT NULL AND t.startDate <= ?1) OR (t.deadline IS NOT NULL AND t.deadline <= ?1))"),
            vec![Sql::Integer(t)], "t.startBucket, t.todayIndex", None)?,
        "upcoming" => query(con, "t.type IN (0,1) AND t.status = 0 AND t.startDate > ?1", vec![Sql::Integer(t)], "t.startDate, t.todayIndex", None)?,
        "anytime" => query(con, &format!("{OPEN_TODO} AND t.start = 1"), vec![], idx, None)?,
        "someday" => query(con, "t.type IN (0,1) AND t.status = 0 AND t.start = 2 AND t.startDate IS NULL AND t.rt1_recurrenceRule IS NULL", vec![], idx, None)?,
        "logbook" => query(con, "t.type IN (0,1) AND t.status IN (2,3)", vec![], "t.stopDate DESC", Some(limit.unwrap_or(50)))?,
        "deadlines" => query(con, "t.type IN (0,1) AND t.status = 0 AND t.deadline IS NOT NULL", vec![], "t.deadline", None)?,
        "repeating" => query(con, "t.rt1_recurrenceRule IS NOT NULL AND t.status = 0", vec![], idx, None)?,
        _ => unreachable!(),
    };
    if let Some(l) = limit {
        r.truncate(l);
    }
    Ok(Value::Array(r))
}

fn show(con: &Connection, id: &str) -> Result<Value> {
    let mut st = con.prepare(&format!("{BASE} AND t.uuid = ?1"))?;
    let found = st.query_row([id], |r| Ok((row_dict(r, true), get::<i64>(r, "type").unwrap_or(0)))).ok();
    let Some((mut d, kind)) = found else {
        let area: Option<String> = con.query_row("SELECT uuid FROM TMArea WHERE uuid = ?1 OR title = ?1 COLLATE NOCASE", [id], |r| r.get(0)).ok();
        let Some(area) = area else { bail!("No item with id {id}") };
        let items = query(con, "t.area = ?1 AND t.status = 0 AND t.type IN (0,1)", vec![Sql::Text(area.clone())], "t.\"index\"", None)?;
        return Ok(json!({ "id": area, "title": id, "type": "area", "items": items }));
    };
    let mut cl = con.prepare("SELECT title, status FROM TMChecklistItem WHERE task = ?1 ORDER BY \"index\"")?;
    let checklist: Vec<Value> = cl.query_map([id], |r| Ok(json!({ "title": r.get::<_, Option<String>>(0)?, "done": r.get::<_, i64>(1)? != 0 })))?
        .collect::<Result<_, _>>()?;
    if !checklist.is_empty() {
        d.insert("checklist".into(), json!(checklist));
    }
    if kind == 1 {
        let items = query(con, "(t.project = ?1 OR h.project = ?1) AND t.status = 0", vec![Sql::Text(id.into())], "h.\"index\", t.\"index\"", None)?;
        d.insert("items".into(), json!(items));
    }
    Ok(Value::Object(d))
}

// ---------- writes ----------

fn token(con: &Connection) -> Result<String> {
    con.query_row("SELECT uriSchemeAuthenticationToken FROM TMSettings", [], |r| r.get::<_, Option<String>>(0))
        .ok().flatten().filter(|t| !t.is_empty())
        .context("No URL auth token. In Things: Settings → General → Enable Things URLs → Manage.")
}

fn open_url(command: &str, params: &[(&str, Option<String>)]) -> Result<()> {
    let q: Vec<String> = params.iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", urlencoding::encode(v))))
        .collect();
    let url = format!("things:///{command}?{}", q.join("&"));
    let st = Command::new("open").args(["-g", &url]).status()?;
    if !st.success() {
        bail!("open {command} failed");
    }
    Ok(())
}

fn wait_for(title: &str, since: f64, kind: i64) -> Result<Option<String>> {
    for _ in 0..20 {
        sleep(Duration::from_millis(300));
        let r = db()?.query_row(
            "SELECT uuid FROM TMTask WHERE title = ?1 AND type = ?2 AND creationDate >= ?3 ORDER BY creationDate DESC LIMIT 1",
            rusqlite::params![title, kind, since - 2.0], |r| r.get::<_, String>(0)).ok();
        if r.is_some() {
            return Ok(r);
        }
    }
    Ok(None)
}

fn joined(v: &[String]) -> Option<String> {
    (!v.is_empty()).then(|| v.join("\n"))
}

fn common_params(w: &WriteArgs) -> Vec<(&'static str, Option<String>)> {
    vec![("notes", w.notes.clone()), ("when", w.when.clone()), ("deadline", w.deadline.clone()), ("tags", w.tags.clone())]
}

fn created(uid: Option<String>, title: &str) {
    match uid {
        Some(u) => println!("{}", json!({ "created": u, "title": title })),
        None => println!("Sent to Things (id not confirmed yet)"),
    }
}

fn update(a: UpdateArgs, fmt: Format) -> Result<()> {
    let con = db()?;
    let kind: i64 = con.query_row("SELECT type FROM TMTask WHERE uuid = ?1", [&a.id], |r| r.get(0)).with_context(|| format!("No item with id {}", a.id))?;
    let mut p = common_params(&a.w);
    p.extend([
        ("id", Some(a.id.clone())), ("auth-token", Some(token(&con)?)), ("title", a.title.clone()),
        ("prepend-notes", a.prepend_notes.clone()), ("append-notes", a.append_notes.clone()), ("add-tags", a.add_tags.clone()),
        ("completed", if a.complete { Some("true".into()) } else if a.reopen { Some("false".into()) } else { None }),
        ("canceled", a.cancel.then(|| "true".into())),
        ("reveal", Some("false".into())),
    ]);
    if kind == 1 {
        p.extend([("area-id", a.list_id.clone()), ("area", a.list.clone())]);
        open_url("update-project", &p)?;
    } else {
        p.extend([
            ("list", a.list.clone()), ("list-id", a.list_id.clone()), ("heading", a.heading.clone()),
            ("checklist-items", joined(&a.checklist)), ("append-checklist-items", joined(&a.append_checklist)),
        ]);
        open_url("update", &p)?;
    }
    sleep(Duration::from_millis(800));
    out(show(&db()?, &a.id)?, fmt);
    Ok(())
}

fn main() {
    common::reset_sigpipe();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("things: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let fmt = cli.format;
    match cli.cmd {
        Cmd::Inbox(l) => out(list(&db()?, "inbox", l.limit)?, fmt),
        Cmd::Today(l) => out(list(&db()?, "today", l.limit)?, fmt),
        Cmd::Upcoming(l) => out(list(&db()?, "upcoming", l.limit)?, fmt),
        Cmd::Anytime(l) => out(list(&db()?, "anytime", l.limit)?, fmt),
        Cmd::Someday(l) => out(list(&db()?, "someday", l.limit)?, fmt),
        Cmd::Logbook(l) => out(list(&db()?, "logbook", l.limit)?, fmt),
        Cmd::Deadlines(l) => out(list(&db()?, "deadlines", l.limit)?, fmt),
        Cmd::Repeating(l) => out(list(&db()?, "repeating", l.limit)?, fmt),
        Cmd::Projects { area } => {
            let (f, p) = match area {
                Some(a) => ("t.type = 1 AND t.status = 0 AND (a.title = ?1 COLLATE NOCASE OR a.uuid = ?1)", vec![Sql::Text(a)]),
                None => ("t.type = 1 AND t.status = 0", vec![]),
            };
            out(Value::Array(query(&db()?, f, p, "a.\"index\", t.\"index\"", None)?), fmt)
        }
        Cmd::Areas => {
            let con = db()?;
            let mut st = con.prepare("SELECT uuid, title FROM TMArea ORDER BY \"index\"")?;
            let v: Vec<Value> = st.query_map([], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "title": r.get::<_, Option<String>>(1)?, "type": "area" })))?.collect::<Result<_, _>>()?;
            out(Value::Array(v), fmt)
        }
        Cmd::Tags => {
            let con = db()?;
            let mut st = con.prepare("SELECT uuid, title FROM TMTag ORDER BY \"index\"")?;
            let v: Vec<Value> = st.query_map([], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "title": r.get::<_, Option<String>>(1)? })))?.collect::<Result<_, _>>()?;
            out(Value::Array(v), fmt)
        }
        Cmd::Search { text, all, limit } => {
            let like = format!("%{text}%");
            let f = if all { "(t.title LIKE ?1 OR t.notes LIKE ?1) AND t.type IN (0,1)" } else { "(t.title LIKE ?1 OR t.notes LIKE ?1) AND t.type IN (0,1) AND t.status = 0" };
            out(Value::Array(query(&db()?, f, vec![Sql::Text(like)], "t.status, t.userModificationDate DESC", Some(limit.unwrap_or(50)))?), fmt)
        }
        Cmd::Tagged { tag } => {
            let f = "t.status = 0 AND t.type IN (0,1) AND t.uuid IN (SELECT tt.tasks FROM TMTaskTag tt JOIN TMTag tg ON tg.uuid = tt.tags WHERE tg.title = ?1 COLLATE NOCASE)";
            out(Value::Array(query(&db()?, f, vec![Sql::Text(tag)], "t.\"index\"", None)?), fmt)
        }
        Cmd::Show { id } => out(show(&db()?, &id)?, fmt),
        Cmd::Reveal { id } => {
            Command::new("open").arg(format!("things:///show?id={}", urlencoding::encode(&id))).status()?;
        }
        Cmd::Add(a) => {
            let since = now();
            let mut p = common_params(&a.w);
            p.extend([
                ("title", Some(a.title.clone())), ("list", a.list), ("list-id", a.list_id), ("heading", a.heading),
                ("checklist-items", joined(&a.checklist)), ("reveal", Some("false".into())),
            ]);
            open_url("add", &p)?;
            created(wait_for(&a.title, since, 0)?, &a.title);
        }
        Cmd::AddProject(a) => {
            let since = now();
            let mut p = common_params(&a.w);
            p.extend([
                ("title", Some(a.title.clone())), ("area", a.area), ("area-id", a.area_id),
                ("to-dos", joined(&a.todo)), ("reveal", Some("false".into())),
            ]);
            open_url("add-project", &p)?;
            created(wait_for(&a.title, since, 1)?, &a.title);
        }
        Cmd::Update(a) => update(a, fmt)?,
        Cmd::Complete { id } => update(UpdateArgs { id, complete: true, ..Default::default() }, fmt)?,
        Cmd::Cancel { id } => update(UpdateArgs { id, cancel: true, ..Default::default() }, fmt)?,
        Cmd::Delete { id } => {
            let con = db()?;
            let (title, kind, status): (Option<String>, i64, i64) = con
                .query_row("SELECT title, type, status FROM TMTask WHERE uuid = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .with_context(|| format!("No item with id {id}"))?;
            if status != 0 {
                // AppleScript can't address logged items; reopen first.
                open_url(if kind == 1 { "update-project" } else { "update" }, &[
                    ("id", Some(id.clone())), ("auth-token", Some(token(&con)?)),
                    ("completed", Some("false".into())), ("canceled", Some("false".into())),
                ])?;
                sleep(Duration::from_millis(800));
            }
            let noun = if kind == 1 { "project" } else { "to do" };
            let script = format!("on run argv\ntell application \"Things3\" to delete {noun} id (item 1 of argv)\nend run");
            if !Command::new("osascript").args(["-e", &script, &id]).status()?.success() {
                bail!("osascript delete failed");
            }
            println!("{}", json!({ "trashed": id, "title": title }));
        }
    }
    Ok(())
}

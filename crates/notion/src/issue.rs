//! Create pages in a Notion database from one of its templates, then set properties
//! and fill the template's sections. Database ids and template aliases come from the
//! `[notion.issue]` section of the local config.

use crate::api::{Notion, V2025, page_id};
use crate::md;
use crate::props::{is_set, plain, value};
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use common::IssueConfig;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const HEADINGS: [&str; 3] = ["heading_1", "heading_2", "heading_3"];

#[derive(Subcommand)]
pub enum IssueCmd {
    /// Templates, their preset properties and section headings
    Templates,
    /// Allowed values for select/status/multi_select properties
    Options { properties: Vec<String> },
    /// Create an issue from a template
    Create(CreateArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Template alias from config (e.g. bug, cr, tech) or its full Notion name
    #[arg(long)]
    template: String,
    #[arg(long)]
    title: String,
    /// Override the Type the template implies
    #[arg(long = "type")]
    kind: Option<String>,
    #[arg(long)]
    priority: Option<String>,
    /// Comma-separated
    #[arg(long)]
    product: Option<String>,
    #[arg(long)]
    discovery: Option<String>,
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    summary: Option<String>,
    #[arg(long)]
    slack: Option<String>,
    #[arg(long)]
    github: Option<String>,
    /// Comma-separated page ids/URLs
    #[arg(long)]
    component: Option<String>,
    /// Comma-separated page ids/URLs
    #[arg(long)]
    project: Option<String>,
    /// Comma-separated page ids/URLs
    #[arg(long)]
    feature: Option<String>,
    /// Any other property, PROP=VALUE (repeatable)
    #[arg(long = "set", value_name = "PROP=VALUE")]
    set: Vec<String>,
    /// Fill a template section; HEADING may be a distinctive substring (repeatable)
    #[arg(long = "section", value_name = "HEADING=MARKDOWN")]
    section: Vec<String>,
    /// JSON file ('-' for stdin) mapping heading -> markdown
    #[arg(long)]
    sections: Option<String>,
    /// Markdown appended at the end of the page ('-' for stdin)
    #[arg(long)]
    body: Option<String>,
    /// Seconds to wait for the template to apply
    #[arg(long, default_value_t = 45)]
    timeout: u64,
    /// Validate and print what would be created
    #[arg(long)]
    dry_run: bool,
}

/// Flag -> property name defaults; `[notion.issue.properties]` overrides them.
const FLAG_PROPS: [(&str, &str); 11] = [
    ("type", "Type"),
    ("priority", "Priority"),
    ("product", "Product"),
    ("discovery", "Discovery context"),
    ("status", "Status"),
    ("summary", "Summary"),
    ("slack", "Slack"),
    ("github", "Github"),
    ("component", "Component"),
    ("project", "Project"),
    ("feature", "Feature"),
];

struct Ctx {
    api: Notion,
    cfg: IssueConfig,
}

impl Ctx {
    fn prop_name(&self, flag: &str) -> String {
        self.cfg.properties.get(flag).cloned().unwrap_or_else(|| {
            FLAG_PROPS.iter().find(|(f, _)| *f == flag).map(|(_, p)| p.to_string()).unwrap_or_else(|| flag.to_string())
        })
    }

    fn schema(&self) -> Result<Map<String, Value>> {
        let ds = self.api.get(&format!("/data_sources/{}", self.cfg.data_source_id))?;
        Ok(ds["properties"].as_object().cloned().unwrap_or_default())
    }

    fn templates(&self) -> Result<Vec<Value>> {
        let r = self.api.get(&format!("/data_sources/{}/templates", self.cfg.data_source_id))?;
        Ok(r["templates"].as_array().cloned().unwrap_or_default())
    }

    /// Type implied by a template, by its Notion name.
    fn implied_type(&self, name: &str) -> Option<String> {
        self.cfg.templates.values().find(|t| norm(&t.name) == norm(name)).and_then(|t| t.kind.clone())
    }

    fn resolve_template(&self, name: &str) -> Result<Value> {
        let templates = self.templates()?;
        let wanted = self.cfg.templates.get(&name.trim().to_lowercase()).map(|t| t.name.clone()).unwrap_or(name.to_string());
        templates.iter().find(|t| norm(t["name"].as_str().unwrap_or("")) == norm(&wanted)).cloned().ok_or_else(|| {
            let names: Vec<_> = templates.iter().filter_map(|t| t["name"].as_str()).collect();
            anyhow!("unknown template '{name}'. Available: {}", names.join(", "))
        })
    }

    /// (preset properties, template blocks, section headings)
    fn details(&self, tid: &str) -> Result<(Map<String, Value>, Vec<Value>, Vec<String>)> {
        let page = self.api.get(&format!("/pages/{tid}"))?;
        let mut presets = Map::new();
        for (k, p) in page["properties"].as_object().into_iter().flatten() {
            // Only user-settable properties count as presets; computed ones (created_by,
            // unique_id, buttons...) would make the "template applied" check meaningless.
            let settable = matches!(p["type"].as_str(), Some(
                "rich_text" | "select" | "status" | "multi_select" | "people" | "relation" | "date" | "checkbox" | "url" | "email"
            ));
            let v = value(p);
            if settable && is_set(&v) {
                presets.insert(k.clone(), v);
            }
        }
        let blocks = self.api.children(tid)?;
        let headings = blocks.iter().filter(|b| is_heading(b)).map(block_text).collect();
        Ok((presets, blocks, headings))
    }
}

fn norm(s: &str) -> String {
    let re = Regex::new(r"[^a-z0-9]+").unwrap();
    re.replace_all(&s.to_lowercase(), " ").trim().to_string()
}

fn is_heading(b: &Value) -> bool {
    HEADINGS.contains(&b["type"].as_str().unwrap_or(""))
}

fn block_text(b: &Value) -> String {
    let t = b["type"].as_str().unwrap_or("");
    plain(&b[t]["rich_text"])
}

fn match_heading(key: &str, headings: &[String]) -> Option<String> {
    let k = norm(key);
    let exact: Vec<_> = headings.iter().filter(|h| norm(h) == k).collect();
    let hits = if exact.is_empty() { headings.iter().filter(|h| !k.is_empty() && norm(h).contains(&k)).collect() } else { exact };
    if hits.len() == 1 { Some(hits[0].clone()) } else { None }
}

fn build_property(name: &str, raw: &str, props: &Map<String, Value>) -> Result<Value> {
    let p = props.get(name).ok_or_else(|| {
        let mut names: Vec<_> = props.keys().cloned().collect();
        names.sort();
        anyhow!("unknown property '{name}'. Properties: {}", names.join(", "))
    })?;
    let t = p["type"].as_str().unwrap_or("");
    let list: Vec<&str> = raw.split(',').map(str::trim).filter(|v| !v.is_empty()).collect();
    if matches!(t, "select" | "status" | "multi_select") {
        let allowed: Vec<&str> = p[t]["options"].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect();
        let values = if t == "multi_select" { list.clone() } else { vec![raw] };
        for v in values {
            if !allowed.contains(&v) {
                bail!("'{v}' is not a valid {name}. Options: {}", allowed.join(", "));
            }
        }
    }
    Ok(match t {
        "title" => json!({ "title": md::rich_text(raw) }),
        "rich_text" => json!({ "rich_text": md::rich_text(raw) }),
        "select" => json!({ "select": { "name": raw } }),
        "status" => json!({ "status": { "name": raw } }),
        "multi_select" => json!({ "multi_select": list.iter().map(|v| json!({ "name": v })).collect::<Vec<_>>() }),
        "checkbox" => json!({ "checkbox": matches!(raw.to_lowercase().as_str(), "1" | "true" | "yes" | "y") }),
        "url" => json!({ "url": raw }),
        "number" => json!({ "number": raw.parse::<f64>().context("not a number")? }),
        "date" => json!({ "date": { "start": raw } }),
        "relation" => json!({ "relation": list.iter().map(|v| page_id(v).map(|id| json!({ "id": id }))).collect::<Result<Vec<_>>>()? }),
        _ => bail!("property '{name}' has type {t}, which can't be set by this command"),
    })
}

fn read_source(arg: &str) -> Result<String> {
    if arg == "-" { common::read_arg("-") } else { std::fs::read_to_string(common::expand(arg)).with_context(|| format!("reading {arg}")) }
}

/// Replace the placeholder content under each heading with the given markdown.
fn fill_sections(api: &Notion, page: &str, sections: &BTreeMap<String, String>) -> Result<()> {
    let blocks = api.children(page)?;
    for (heading, markdown) in sections {
        let idx = blocks.iter().position(|b| is_heading(b) && &block_text(b) == heading).unwrap();
        // Placeholder region: everything after the heading until the next heading or divider.
        for b in blocks[idx + 1..].iter().take_while(|b| !is_heading(b) && b["type"] != "divider") {
            api.call("DELETE", &format!("/blocks/{}", b["id"].as_str().unwrap()), None)?;
        }
        api.append(page, &md::to_blocks(markdown), blocks[idx]["id"].as_str().map(String::from))?;
    }
    Ok(())
}

pub fn run(cmd: IssueCmd) -> Result<()> {
    let cfg = common::load()?.notion.issue.ok_or_else(|| {
        anyhow!("no [notion.issue] section in {}; see config.example.toml", common::config_path().display())
    })?;
    let ctx = Ctx { api: Notion::new(V2025)?, cfg };
    match cmd {
        IssueCmd::Templates => {
            for t in ctx.templates()? {
                let name = t["name"].as_str().unwrap_or("");
                let (presets, _, headings) = ctx.details(t["id"].as_str().unwrap_or(""))?;
                let implies = ctx.implied_type(name).unwrap_or_else(|| "none, pass --type".into());
                let aliases: Vec<_> = ctx.cfg.templates.iter().filter(|(_, c)| norm(&c.name) == norm(name)).map(|(a, _)| a.as_str()).collect();
                println!("## {name}   (alias: {}; implies Type: {implies})", if aliases.is_empty() { "-".into() } else { aliases.join(", ") });
                println!("   presets: {}", Value::Object(presets));
                println!("   sections: {}", if headings.is_empty() { "(none, use --body)".into() } else { headings.join(" | ") });
            }
        }
        IssueCmd::Options { properties } => {
            let props = ctx.schema()?;
            let names: Vec<String> = if properties.is_empty() {
                props.iter().filter(|(_, p)| matches!(p["type"].as_str(), Some("select" | "status" | "multi_select"))).map(|(k, _)| k.clone()).collect()
            } else {
                properties
            };
            for n in names {
                let p = props.get(&n).ok_or_else(|| anyhow!("unknown property '{n}'"))?;
                let t = p["type"].as_str().unwrap_or("");
                let opts: Vec<&str> = p[t]["options"].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect();
                println!("{n} ({t}){}", if opts.is_empty() { String::new() } else { format!(": {}", opts.join(" | ")) });
            }
        }
        IssueCmd::Create(a) => create(&ctx, a)?,
    }
    Ok(())
}

fn create(ctx: &Ctx, a: CreateArgs) -> Result<()> {
    let tmpl = ctx.resolve_template(&a.template)?;
    let tname = tmpl["name"].as_str().unwrap_or("").to_string();
    let tid = tmpl["id"].as_str().unwrap_or("").to_string();
    let (presets, tblocks, headings) = ctx.details(&tid)?;
    let props = ctx.schema()?;
    let title_prop = props.iter().find(|(_, p)| p["type"] == "title").map(|(k, _)| k.clone()).unwrap_or("Name".into());

    let mut raw: Vec<(String, String)> = Vec::new();
    if let Some(src) = &a.sections {
        let v: Map<String, Value> = serde_json::from_str(&read_source(src)?).context("--sections must be a JSON object")?;
        raw.extend(v.into_iter().map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string())));
    }
    for s in &a.section {
        let (k, v) = s.split_once('=').ok_or_else(|| anyhow!("--section expects 'Heading=markdown', got '{s}'"))?;
        raw.push((k.to_string(), v.replace("\\n", "\n")));
    }
    let mut sections = BTreeMap::new();
    for (k, v) in raw {
        let h = match_heading(&k, &headings).ok_or_else(|| {
            anyhow!("section '{k}' doesn't match one heading of template '{tname}'. Headings: {}",
                if headings.is_empty() { "(none, use --body)".into() } else { headings.join(" | ") })
        })?;
        sections.insert(h, v);
    }

    // Properties are set after the template applies, so they win over its presets.
    let mut wanted: Vec<(String, String)> = Vec::new();
    let type_prop = ctx.prop_name("type");
    let sets: Vec<(String, String)> = a.set.iter().map(|kv| {
        kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| anyhow!("--set expects 'Property=value', got '{kv}'"))
    }).collect::<Result<_>>()?;
    match a.kind.clone().or_else(|| ctx.implied_type(&tname)) {
        Some(t) => wanted.push((type_prop, t)),
        None if !sets.iter().any(|(k, _)| *k == type_prop) => {
            let opts: Vec<&str> = props.get(&type_prop).map(|p| p["select"]["options"].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect()).unwrap_or_default();
            bail!("template '{tname}' sets no {type_prop}; pass --type ({})", opts.join(" | "));
        }
        None => {}
    }
    let flags = [
        ("priority", &a.priority), ("product", &a.product), ("discovery", &a.discovery), ("status", &a.status),
        ("summary", &a.summary), ("slack", &a.slack), ("github", &a.github), ("component", &a.component),
        ("project", &a.project), ("feature", &a.feature),
    ];
    for (flag, v) in flags {
        if let Some(v) = v {
            wanted.push((ctx.prop_name(flag), v.clone()));
        }
    }
    wanted.extend(sets);
    let mut properties = Map::new();
    for (k, v) in &wanted {
        properties.insert(k.clone(), build_property(k, v, &props)?);
    }
    let reporter_prop = ctx.prop_name("reporter");
    let reporter_prop = if reporter_prop == "reporter" { "Reporter".to_string() } else { reporter_prop };
    if let Some(rid) = &ctx.cfg.reporter_id {
        properties.insert(reporter_prop.clone(), json!({ "people": [{ "id": rid }] }));
    }
    properties.insert(title_prop.clone(), json!({ "title": md::rich_text(&a.title) }));

    if a.dry_run {
        let mut fin = presets.clone();
        for (k, v) in &wanted {
            fin.insert(k.clone(), json!(v));
        }
        if ctx.cfg.reporter_id.is_some() {
            fin.insert(reporter_prop, json!([ctx.cfg.reporter_name.clone().unwrap_or("(configured reporter)".into())]));
        }
        fin.insert(title_prop, json!(a.title));
        let unfilled: Vec<_> = headings.iter().filter(|h| !sections.contains_key(*h)).collect();
        println!("{}", serde_json::to_string_pretty(&json!({
            "template": tname, "properties": fin, "sections": sections, "body": a.body, "unfilled_sections": unfilled,
        }))?);
        return Ok(());
    }

    let page = ctx.api.call("POST", "/pages", Some(&json!({
        "parent": { "type": "data_source_id", "data_source_id": ctx.cfg.data_source_id },
        "properties": { &title_prop: properties[&title_prop] },
        "template": { "type": "template_id", "template_id": tid },
    })))?;
    let pid = page["id"].as_str().unwrap_or("").to_string();
    let url = page["url"].as_str().unwrap_or("").to_string();

    // Template application is async: wait until its blocks and preset properties land.
    let deadline = Instant::now() + Duration::from_secs(a.timeout);
    loop {
        let cur = ctx.api.get(&format!("/pages/{pid}"))?;
        let props_done = presets.keys().all(|k| cur["properties"].get(k).is_none_or(|p| is_set(&value(p))));
        let blocks_done = tblocks.is_empty() || ctx.api.children(&pid)?.len() >= tblocks.len();
        if props_done && blocks_done {
            break;
        }
        if Instant::now() > deadline {
            bail!("template still applying after {}s; page created at {url} but properties/sections NOT set. Don't re-run (it would duplicate); finish it by hand.", a.timeout);
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    ctx.api.call("PATCH", &format!("/pages/{pid}"), Some(&json!({ "properties": properties })))?;
    if !sections.is_empty() {
        fill_sections(&ctx.api, &pid, &sections)?;
    }
    if let Some(body) = &a.body {
        let body = if body == "-" { common::read_arg("-")? } else { body.clone() };
        ctx.api.append(&pid, &md::to_blocks(&body), None)?;
    }
    println!("{url}");
    let unfilled: Vec<_> = headings.iter().filter(|h| !sections.contains_key(*h)).cloned().collect();
    if !unfilled.is_empty() {
        eprintln!("note: left template placeholder text in: {}", unfilled.join(" | "));
    }
    Ok(())
}

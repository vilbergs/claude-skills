//! notion — query Notion databases, read pages as markdown, write blocks, file issues.
//!
//! Auth: NOTION_TOKEN, else the token file from config (default ~/.notion_token).

mod api;
mod issue;
mod md;
mod props;
mod render;

use anyhow::{Context, Result, bail};
use api::{Notion, V2022, page_id};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "notion", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Find databases (or pages with --pages) the integration can see: "<id>\t<title>"
    Search {
        query: Option<String>,
        #[arg(long)]
        pages: bool,
    },
    /// A database's property names and types
    Schema { database: String },
    /// Query a database; one flattened JSON object per row. BODY is JSON, a file path, @file or -
    Query {
        database: String,
        body: Option<String>,
        /// Print the unflattened API results as a JSON array
        #[arg(long)]
        raw: bool,
        /// Stop after this many rows (default: all)
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Fetch pages by id/URL and flatten their properties (resolves relation ids)
    Page { ids: Vec<String> },
    /// Render a page (or any block) and its children as markdown
    Read {
        id: String,
        /// How deep to follow nested blocks
        #[arg(long, default_value_t = 10)]
        depth: usize,
        /// Tag each block with its id as an HTML comment (for update/replace/delete)
        #[arg(long)]
        ids: bool,
    },
    /// Append markdown as blocks under a page or block. MARKDOWN is text, @file or -
    Append {
        parent: String,
        markdown: String,
        /// Insert after this child block instead of at the end
        #[arg(long)]
        after: Option<String>,
    },
    /// Create a page under a page (or a database row with --database) with markdown content
    CreatePage {
        #[arg(long)]
        parent: String,
        #[arg(long)]
        title: String,
        /// The parent is a database: create a row
        #[arg(long)]
        database: bool,
        /// Optional content: text, @file or -
        markdown: Option<String>,
    },
    /// Change a block's text in place (same block type). MARKDOWN is text, @file or -
    Update { block: String, markdown: String },
    /// Replace a block with new markdown content (any number/type of blocks)
    Replace { block: String, markdown: String },
    /// Delete blocks. Refuses child pages/databases unless --force (that trashes the page)
    Delete {
        blocks: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Delete all content of a page, keeping child pages and databases
    Clear { page: String },
    /// Set page properties: PROP=VALUE pairs (select/status/multi_select/text/number/date/checkbox/url)
    SetProps { page: String, pairs: Vec<String> },
    /// Raw API call; prints the JSON response. BODY is JSON, @file or -
    Api {
        method: String,
        path: String,
        body: Option<String>,
        #[arg(long, default_value = V2022)]
        version: String,
    },
    /// Template-based issues in the configured database
    Issue {
        #[command(subcommand)]
        cmd: issue::IssueCmd,
    },
}

/// JSON body argument: inline JSON, a path to a file, @file or -.
fn body_arg(arg: Option<&str>) -> Result<Value> {
    let Some(a) = arg else { return Ok(Value::Null) };
    let text = if a != "-" && !a.starts_with('@') && std::path::Path::new(a).is_file() {
        std::fs::read_to_string(a)?
    } else {
        common::read_arg(a)?
    };
    serde_json::from_str(&text).context("body is not valid JSON")
}

fn print_lines(rows: impl IntoIterator<Item = Value>) {
    for r in rows {
        println!("{r}");
    }
}

fn main() {
    common::reset_sigpipe();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("notion: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Cmd::Issue { cmd } = cli.cmd {
        return issue::run(cmd);
    }
    let api = Notion::new(V2022)?;
    match cli.cmd {
        Cmd::Search { query, pages } => {
            let kind = if pages { "page" } else { "database" };
            let body = json!({ "query": query.unwrap_or_default(), "filter": { "value": kind, "property": "object" } });
            for r in api.query_all("/search", &body, None)? {
                println!("{}\t{}", r["id"].as_str().unwrap_or(""), props::title_of(&r));
            }
        }
        Cmd::Schema { database } => {
            let db = api.get(&format!("/databases/{}", page_id(&database)?))?;
            let mut rows: Vec<String> = db["properties"].as_object().into_iter().flatten()
                .map(|(k, p)| format!("{k}\t{}", p["type"].as_str().unwrap_or(""))).collect();
            rows.sort();
            rows.iter().for_each(|r| println!("{r}"));
        }
        Cmd::Query { database, body, raw, limit } => {
            let rows = api.query_all(&format!("/databases/{}/query", page_id(&database)?), &body_arg(body.as_deref())?, limit)?;
            if raw {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print_lines(rows.iter().map(props::flatten));
            }
        }
        Cmd::Page { ids } => {
            if ids.is_empty() {
                bail!("page: at least one page id required");
            }
            for id in ids {
                println!("{}", props::flatten(&api.get(&format!("/pages/{}", page_id(&id)?))?));
            }
        }
        Cmd::Read { id, depth, ids } => {
            let id = page_id(&id)?;
            let r = render::Render { api: &api, max_depth: depth, ids };
            if let Ok(page) = api.get(&format!("/pages/{id}")) {
                println!("# {}\n", props::title_of(&page));
            }
            println!("{}", r.page(&id)?);
        }
        Cmd::Append { parent, markdown, after } => {
            let blocks = md::to_blocks(&common::read_arg(&markdown)?);
            let after = after.map(|a| page_id(&a)).transpose()?;
            let ids = api.append(&page_id(&parent)?, &blocks, after)?;
            println!("appended {} block(s)", ids.len());
        }
        Cmd::CreatePage { parent, title, database, markdown } => {
            let parent = page_id(&parent)?;
            let (parent_obj, title_prop) = if database {
                let db = api.get(&format!("/databases/{parent}"))?;
                let tp = db["properties"].as_object().into_iter().flatten()
                    .find(|(_, p)| p["type"] == "title").map(|(k, _)| k.clone()).unwrap_or("Name".into());
                (json!({ "database_id": parent }), tp)
            } else {
                (json!({ "page_id": parent }), "title".to_string())
            };
            let page = api.call("POST", "/pages", Some(&json!({
                "parent": parent_obj,
                "properties": { title_prop: { "title": md::rich_text(&title) } },
            })))?;
            if let Some(m) = markdown {
                api.append(page["id"].as_str().unwrap_or(""), &md::to_blocks(&common::read_arg(&m)?), None)?;
            }
            println!("{}", page["url"].as_str().unwrap_or(""));
        }
        Cmd::Update { block, markdown } => {
            let id = page_id(&block)?;
            let cur = api.get(&format!("/blocks/{id}"))?;
            let kind = cur["type"].as_str().unwrap_or("").to_string();
            let new = md::to_blocks(&common::read_arg(&markdown)?);
            let [nb] = new.as_slice() else { bail!("update takes exactly one block of markdown; use replace for more") };
            let nkind = nb["type"].as_str().unwrap_or("");
            // A paragraph's text can go into any text block (keeps e.g. a bullet a bullet).
            let mut payload = if nkind == kind || nkind == "paragraph" {
                json!({ "rich_text": nb[nkind]["rich_text"] })
            } else {
                bail!("block is a {kind} but the markdown is a {nkind}; use replace to change the type");
            };
            if kind == "to_do" && nkind == "to_do" {
                payload["checked"] = nb["to_do"]["checked"].clone();
            }
            if kind == "code" && nkind == "code" {
                payload["language"] = nb["code"]["language"].clone();
            }
            api.call("PATCH", &format!("/blocks/{id}"), Some(&json!({ kind: payload })))?;
            println!("updated {id}");
        }
        Cmd::Replace { block, markdown } => {
            let id = page_id(&block)?;
            let cur = api.get(&format!("/blocks/{id}"))?;
            let p = &cur["parent"];
            let parent = p[p["type"].as_str().unwrap_or("")].as_str().unwrap_or("").to_string();
            let ids = api.append(&parent, &md::to_blocks(&common::read_arg(&markdown)?), Some(id.clone()))?;
            api.call("DELETE", &format!("/blocks/{id}"), None)?;
            println!("replaced {id} with {} block(s)", ids.len());
        }
        Cmd::Delete { blocks, force } => {
            for b in blocks {
                let id = page_id(&b)?;
                if !force {
                    let cur = api.get(&format!("/blocks/{id}"))?;
                    if matches!(cur["type"].as_str(), Some("child_page" | "child_database")) {
                        bail!("{id} is a {}; deleting it trashes the whole page. Pass --force if that is intended", cur["type"]);
                    }
                }
                api.call("DELETE", &format!("/blocks/{id}"), None)?;
                println!("deleted {id}");
            }
        }
        Cmd::Clear { page } => {
            let mut n = 0;
            for b in api.children(&page_id(&page)?)? {
                if !matches!(b["type"].as_str(), Some("child_page" | "child_database")) {
                    api.call("DELETE", &format!("/blocks/{}", b["id"].as_str().unwrap_or("")), None)?;
                    n += 1;
                }
            }
            println!("deleted {n} block(s)");
        }
        Cmd::SetProps { page, pairs } => {
            let id = page_id(&page)?;
            let cur = api.get(&format!("/pages/{id}"))?;
            let mut out = serde_json::Map::new();
            for kv in pairs {
                let (k, v) = kv.split_once('=').with_context(|| format!("expected PROP=VALUE, got '{kv}'"))?;
                let t = cur["properties"][k]["type"].as_str().with_context(|| format!("page has no property '{k}'"))?;
                let list = || v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| json!({ "name": s })).collect::<Vec<_>>();
                let val = match t {
                    "title" | "rich_text" => json!({ t: md::rich_text(v) }),
                    "select" | "status" => if v.is_empty() { json!({ t: null }) } else { json!({ t: { "name": v } }) },
                    "multi_select" => json!({ t: list() }),
                    "number" => json!({ t: v.parse::<f64>().ok() }),
                    "checkbox" => json!({ t: matches!(v, "1" | "true" | "yes") }),
                    "date" => if v.is_empty() { json!({ t: null }) } else { json!({ t: { "start": v } }) },
                    "url" | "email" | "phone_number" => json!({ t: if v.is_empty() { Value::Null } else { json!(v) } }),
                    "relation" => json!({ t: v.split(',').filter(|s| !s.trim().is_empty()).map(|s| page_id(s).map(|id| json!({ "id": id }))).collect::<Result<Vec<_>>>()? }),
                    _ => bail!("property '{k}' has type {t}; use `notion api PATCH /pages/{id}`"),
                };
                out.insert(k.to_string(), val);
            }
            let r = api.call("PATCH", &format!("/pages/{id}"), Some(&json!({ "properties": out })))?;
            println!("{}", props::flatten(&r));
        }
        Cmd::Api { method, path, body, version } => {
            let api = Notion { version, ..api };
            let path = if path.starts_with('/') || path.starts_with("http") { path } else { format!("/{path}") };
            let body = body_arg(body.as_deref())?;
            let r = api.call(&method.to_uppercase(), &path, (!body.is_null()).then_some(&body))?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        Cmd::Issue { .. } => unreachable!(),
    }
    Ok(())
}

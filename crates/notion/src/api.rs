use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;
use std::thread::sleep;
use std::time::Duration;

const API: &str = "https://api.notion.com/v1";
pub const V2022: &str = "2022-06-28";
/// Templates and data sources need >= 2025-09-03.
pub const V2025: &str = "2025-09-03";

pub struct Notion {
    pub(crate) token: String,
    pub version: String,
}

impl Notion {
    pub fn new(version: &str) -> Result<Self> {
        Ok(Self { token: token()?, version: version.to_string() })
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{API}{path}") };
        for attempt in 0..5 {
            let req = ureq::request(method, &url)
                .set("Authorization", &format!("Bearer {}", self.token))
                .set("Notion-Version", &self.version)
                .set("Content-Type", "application/json");
            let res = match body {
                Some(b) => req.send_json(b),
                None => req.call(),
            };
            match res {
                Ok(r) => return Ok(r.into_json()?),
                Err(ureq::Error::Status(429, r)) if attempt < 4 => {
                    let wait = r.header("Retry-After").and_then(|s| s.parse().ok()).unwrap_or(1 + attempt);
                    sleep(Duration::from_secs(wait));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let v: Value = r.into_json().unwrap_or(Value::Null);
                    bail!(
                        "Notion API error: {} {} {}",
                        v["status"].as_u64().unwrap_or(code as u64),
                        v["code"].as_str().unwrap_or(""),
                        v["message"].as_str().unwrap_or("")
                    );
                }
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!()
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.call("GET", path, None)
    }

    /// All children of a block or page, following pagination.
    pub fn children(&self, id: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let q = match &cursor {
                Some(c) => format!("/blocks/{id}/children?page_size=100&start_cursor={c}"),
                None => format!("/blocks/{id}/children?page_size=100"),
            };
            let r = self.get(&q)?;
            out.extend(r["results"].as_array().cloned().unwrap_or_default());
            if r["has_more"] != json!(true) {
                return Ok(out);
            }
            cursor = r["next_cursor"].as_str().map(String::from);
        }
    }

    /// POST a query endpoint repeatedly until `has_more` is false; returns all results.
    pub fn query_all(&self, path: &str, body: &Value, limit: Option<usize>) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut body = if body.is_null() { json!({}) } else { body.clone() };
        loop {
            let r = self.call("POST", path, Some(&body))?;
            out.extend(r["results"].as_array().cloned().unwrap_or_default());
            if let Some(n) = limit.filter(|n| out.len() >= *n) {
                out.truncate(n);
                return Ok(out);
            }
            if r["has_more"] != json!(true) {
                return Ok(out);
            }
            body["start_cursor"] = r["next_cursor"].clone();
        }
    }

    /// Append blocks under `parent`, optionally after a sibling. Nested children are
    /// created level by level, so any depth works (the API itself allows only two).
    /// Returns the ids of the created top-level blocks.
    pub fn append(&self, parent: &str, blocks: &[Value], mut after: Option<String>) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for chunk in blocks.chunks(100) {
            let mut nested = Vec::new();
            let stripped: Vec<Value> = chunk
                .iter()
                .map(|b| {
                    let mut b = b.clone();
                    let kind = b["type"].as_str().unwrap_or("").to_string();
                    // Tables must be created with their rows; everything else gets children afterwards.
                    let kids = if kind == "table" { None } else { b[&kind].as_object_mut().and_then(|o| o.remove("children")) };
                    nested.push(kids);
                    b
                })
                .collect();
            let mut body = json!({ "children": stripped });
            if let Some(a) = &after {
                body["after"] = json!(a);
            }
            let r = self.call("PATCH", &format!("/blocks/{parent}/children"), Some(&body))?;
            let created = r["results"].as_array().cloned().unwrap_or_default();
            // With `after`, the response lists the whole parent; the new blocks follow the anchor.
            let created: Vec<Value> = match &after {
                Some(a) => {
                    let pos = created.iter().position(|b| b["id"].as_str() == Some(a.as_str()));
                    created.into_iter().skip(pos.map_or(0, |p| p + 1)).take(chunk.len()).collect()
                }
                None => created.into_iter().rev().take(chunk.len()).collect::<Vec<_>>().into_iter().rev().collect(),
            };
            for (b, kids) in created.iter().zip(nested) {
                let id = b["id"].as_str().unwrap_or_default().to_string();
                if let Some(Value::Array(kids)) = kids {
                    self.append(&id, &kids, None)?;
                }
                ids.push(id);
            }
            if after.is_some() {
                after = ids.last().cloned();
            }
        }
        Ok(ids)
    }
}

fn token() -> Result<String> {
    if let Ok(t) = std::env::var("NOTION_TOKEN")
        && !t.trim().is_empty() {
            return Ok(t.trim().to_string());
        }
    let cfg = common::load()?;
    let path = common::expand(cfg.notion.token_file.as_deref().unwrap_or("~/.notion_token"));
    std::fs::read_to_string(&path)
        .map(|s| s.split_whitespace().collect())
        .map_err(|_| {
            anyhow!(
                "No Notion token: put it in {} or set NOTION_TOKEN. Create an internal integration at https://www.notion.so/my-integrations",
                path.display()
            )
        })
}

static ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([0-9a-f]{32})(?:[?#].*)?$").unwrap());

/// Accept a bare id (with or without dashes) or a Notion URL.
pub fn page_id(reference: &str) -> Result<String> {
    let flat = reference.trim().replace('-', "");
    let caps = ID_RE.captures(&flat).ok_or_else(|| anyhow!("not a Notion page id or URL: {reference}"))?;
    let h = &caps[1];
    Ok(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

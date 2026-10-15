//! Thin wrappers over the `gh` CLI, which owns auth.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

/// Run `gh` with args, optionally feeding stdin; returns stdout.
pub fn gh(args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut child = Command::new("gh")
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running gh (is the GitHub CLI installed?)")?;
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("gh {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8(out.stdout)?)
}

pub fn gh_json(args: &[&str], stdin: Option<&str>) -> Result<Value> {
    let s = gh(args, stdin)?;
    if s.trim().is_empty() { Ok(Value::Null) } else { Ok(serde_json::from_str(&s)?) }
}

/// GraphQL query with retries (GitHub 502s on heavy queries).
pub fn graphql(query: &str, vars: Value, attempts: u32, backoff: u64) -> Result<Value> {
    let body = json!({ "query": query, "variables": vars }).to_string();
    let mut last = None;
    for attempt in 1..=attempts {
        match gh_json(&["api", "graphql", "--input", "-"], Some(&body)) {
            Ok(v) if v["errors"].is_null() => return Ok(v["data"].clone()),
            Ok(v) => last = Some(anyhow!("GraphQL errors: {}", v["errors"])),
            Err(e) => last = Some(e),
        }
        if attempt < attempts {
            eprintln!("  retry {attempt} (GitHub 502s on heavy queries)...");
            sleep(Duration::from_secs(attempt as u64 * backoff));
        }
    }
    Err(last.unwrap())
}

pub fn me() -> Result<String> {
    Ok(gh(&["api", "user", "--jq", ".login"], None)?.trim().to_string())
}

/// Resolve "owner/name" from -R, else the current directory's repo.
pub fn repo(r: Option<&str>) -> Result<String> {
    match r {
        Some(r) => Ok(r.to_string()),
        None => Ok(gh(&["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"], None)
            .context("not in a GitHub repo; pass -R owner/name")?
            .trim()
            .to_string()),
    }
}

/// Accept "123", "#123" or a PR URL (which also fixes the repo).
pub fn pr_ref(pr: &str, r: Option<&str>) -> Result<(String, u64)> {
    let url = Regex::new(r"github\.com/([^/]+/[^/]+)/pull/(\d+)").unwrap();
    if let Some(c) = url.captures(pr) {
        return Ok((c[1].to_string(), c[2].parse()?));
    }
    let n = pr.trim_start_matches('#').parse().with_context(|| format!("not a PR number or URL: {pr}"))?;
    Ok((repo(r)?, n))
}

pub fn days_since(ts: &str) -> i64 {
    DateTime::parse_from_rfc3339(ts).map(|t| (Utc::now() - t.with_timezone(&Utc)).num_seconds().div_euclid(86400)).unwrap_or(0)
}

pub fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

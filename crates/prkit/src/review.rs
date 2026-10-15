//! Reading a PR's review conversation and posting reviews/replies.

use crate::gh::{gh, gh_json, graphql, me, s};
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const THREADS_Q: &str = r#"
query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $n) {
      number title url state isDraft author { login } baseRefName headRefName
      reviews(first: 100) { nodes { databaseId author { login } state body submittedAt } }
      comments(first: 100) { nodes { databaseId author { login } body createdAt } }
      reviewThreads(first: 100) {
        nodes {
          isResolved isOutdated path line originalLine startLine
          comments(first: 100) { nodes { databaseId author { login } body createdAt } }
        }
      }
    }
  }
}"#;

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/').context("repo must be owner/name")
}

pub fn fetch(repo: &str, n: u64) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    let d = graphql(THREADS_Q, json!({ "owner": owner, "name": name, "n": n }), 3, 2)?;
    Ok(d["repository"]["pullRequest"].clone())
}

fn login(v: &Value) -> &str {
    v["author"]["login"].as_str().unwrap_or("ghost")
}

fn date(v: &str) -> &str {
    v.get(..10).unwrap_or(v)
}

fn quote_body(body: &str, ind: &str) -> String {
    body.trim().lines().map(|l| format!("{ind}{l}")).collect::<Vec<_>>().join("\n")
}

fn is_bot(v: &Value) -> bool {
    let l = login(v).to_lowercase();
    l.contains("[bot]") || l.starts_with("coderabbit") || l.starts_with("copilot") || l.starts_with("github-actions")
}

/// Bot bodies (CodeRabbit etc.) are long; collapse them unless asked.
fn body_of(v: &Value, ind: &str, bots: bool) -> String {
    let body = s(&v["body"]);
    if bots || !is_bot(v) {
        return quote_body(body, ind);
    }
    let first = body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('<') && !l.starts_with("> [!")).unwrap_or("");
    format!("{ind}{} [bot, {} lines; --bots to show]", first.chars().take(160).collect::<String>(), body.lines().count())
}

/// Markdown digest: review summaries, conversation, then inline threads grouped by file.
pub fn threads(repo: &str, n: u64, unresolved_only: bool, as_json: bool, bots: bool) -> Result<()> {
    let pr = fetch(repo, n)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&pr)?);
        return Ok(());
    }
    println!("# {repo}#{n} {}\n{}\nby @{} · {} · {} ← {}\n", s(&pr["title"]), s(&pr["url"]), login(&pr), s(&pr["state"]), s(&pr["baseRefName"]), s(&pr["headRefName"]));

    let reviews: Vec<&Value> = pr["reviews"]["nodes"].as_array().into_iter().flatten()
        .filter(|r| !s(&r["body"]).trim().is_empty() || s(&r["state"]) != "COMMENTED").collect();
    if !reviews.is_empty() {
        println!("## Reviews");
        for r in reviews {
            println!("- **@{}** {} ({}, review {})", login(r), s(&r["state"]), date(s(&r["submittedAt"])), r["databaseId"]);
            if !s(&r["body"]).trim().is_empty() {
                println!("{}", body_of(r, "  > ", bots));
            }
        }
        println!();
    }
    let comments = pr["comments"]["nodes"].as_array().cloned().unwrap_or_default();
    if !comments.is_empty() {
        println!("## Conversation");
        for c in &comments {
            println!("- **@{}** ({}, comment {})", login(c), date(s(&c["createdAt"])), c["databaseId"]);
            println!("{}", body_of(c, "  > ", bots));
        }
        println!();
    }

    let mut by_file: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let all = pr["reviewThreads"]["nodes"].as_array().cloned().unwrap_or_default();
    for t in &all {
        if unresolved_only && t["isResolved"] == true {
            continue;
        }
        by_file.entry(s(&t["path"]).to_string()).or_default().push(t);
    }
    let open = all.iter().filter(|t| t["isResolved"] != true).count();
    println!("## Inline threads: {} total, {open} unresolved{}", all.len(), if unresolved_only { " (showing unresolved)" } else { "" });
    for (path, mut ts) in by_file {
        ts.sort_by_key(|t| t["line"].as_u64().or(t["originalLine"].as_u64()).unwrap_or(0));
        println!("\n### {path}");
        for t in ts {
            let line = match (t["startLine"].as_u64(), t["line"].as_u64()) {
                (Some(a), Some(b)) if a != b => format!("L{a}-{b}"),
                (_, Some(b)) => format!("L{b}"),
                _ => format!("L{} (orig)", t["originalLine"]),
            };
            let mut flags = Vec::new();
            if t["isResolved"] == true { flags.push("resolved") }
            if t["isOutdated"] == true { flags.push("outdated") }
            let flags = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };
            println!("- {line}{flags}");
            for c in t["comments"]["nodes"].as_array().into_iter().flatten() {
                println!("  - **@{}** ({}, id {})", login(c), date(s(&c["createdAt"])), c["databaseId"]);
                println!("{}", body_of(c, "    > ", bots));
            }
        }
    }
    Ok(())
}

/// Lines commentable on the RIGHT side per file, from the unified diff.
pub fn diff_lines(repo: &str, n: u64) -> Result<BTreeMap<String, BTreeSet<u64>>> {
    let diff = gh(&["pr", "diff", &n.to_string(), "-R", repo], None)?;
    let hunk = Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@").unwrap();
    let mut out: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut file = String::new();
    let mut line = 0u64;
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("+++ ") {
            file = p.strip_prefix("b/").unwrap_or(p).to_string();
        } else if l.starts_with("--- ") || l.starts_with("diff --git") {
        } else if let Some(c) = hunk.captures(l) {
            line = c[1].parse()?;
        } else if l.starts_with('+') || l.starts_with(' ') {
            out.entry(file.clone()).or_default().insert(line);
            line += 1;
        }
    }
    Ok(out)
}

/// Post one review from a draft directory:
///   summary.md        review body (optional)
///   anchors.tsv       "<id>\t<path>\t<line>" or "<id>\t<path>\t<start>-<end>" per comment
///   <id>.md           body of each inline comment
pub fn submit(repo: &str, n: u64, dir: &Path, event: &str, dry_run: bool) -> Result<()> {
    let event = event.to_uppercase();
    if !matches!(event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES") {
        bail!("--event must be COMMENT, APPROVE or REQUEST_CHANGES");
    }
    let summary = std::fs::read_to_string(dir.join("summary.md")).unwrap_or_default();
    let anchors = std::fs::read_to_string(dir.join("anchors.tsv")).unwrap_or_default();
    let valid = diff_lines(repo, n)?;
    let mut comments = Vec::new();
    let mut problems = Vec::new();
    for row in anchors.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let cols: Vec<&str> = row.split('\t').collect();
        let [id, path, lines] = cols[..] else {
            problems.push(format!("bad anchors.tsv row (want id<TAB>path<TAB>line): {row}"));
            continue;
        };
        let body = std::fs::read_to_string(dir.join(format!("{id}.md"))).with_context(|| format!("missing {id}.md"))?;
        let (start, end) = match lines.split_once('-') {
            Some((a, b)) => (Some(a.trim().parse::<u64>()?), b.trim().parse::<u64>()?),
            None => (None, lines.trim().parse::<u64>()?),
        };
        let ok = |l: u64| valid.get(path).is_some_and(|set| set.contains(&l));
        if !valid.contains_key(path) {
            problems.push(format!("{id}: {path} is not in the diff"));
        } else if !ok(end) || start.is_some_and(|s| !ok(s)) {
            problems.push(format!("{id}: {path}:{lines} is not a line in the diff (new-file side)"));
        }
        let mut c = json!({ "path": path, "line": end, "side": "RIGHT", "body": body.trim_end() });
        if let Some(s) = start {
            c["start_line"] = json!(s);
            c["start_side"] = json!("RIGHT");
        }
        comments.push(c);
    }
    if !problems.is_empty() {
        bail!("GitHub would reject this review:\n  {}", problems.join("\n  "));
    }
    let payload = json!({ "body": summary.trim_end(), "event": event, "comments": comments });
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        eprintln!("dry run: {} inline comment(s), event {event}; all anchors are in the diff", comments.len());
        return Ok(());
    }
    let r = gh_json(&["api", "-X", "POST", &format!("repos/{repo}/pulls/{n}/reviews"), "--input", "-"], Some(&payload.to_string()))?;
    println!("{}", s(&r["html_url"]));
    Ok(())
}

pub fn reply(repo: &str, n: u64, comment: u64, body: &str) -> Result<()> {
    let payload = json!({ "body": body.trim_end() }).to_string();
    let r = gh_json(&["api", "-X", "POST", &format!("repos/{repo}/pulls/{n}/comments/{comment}/replies"), "--input", "-"], Some(&payload))?;
    println!("{}", s(&r["html_url"]));
    Ok(())
}

/// List (or delete) my pending, unsubmitted reviews on a PR.
pub fn pending(repo: &str, n: u64, delete: bool) -> Result<()> {
    let me = me()?;
    let reviews = gh_json(&["api", &format!("repos/{repo}/pulls/{n}/reviews?per_page=100")], None)?;
    let mine: Vec<&Value> = reviews.as_array().into_iter().flatten()
        .filter(|r| r["state"] == "PENDING" && r["user"]["login"] == me.as_str()).collect();
    if mine.is_empty() {
        println!("no pending review by @{me}");
    }
    for r in mine {
        let id = r["id"].as_u64().unwrap_or(0);
        if delete {
            gh(&["api", "-X", "DELETE", &format!("repos/{repo}/pulls/{n}/reviews/{id}")], None)?;
            println!("deleted pending review {id}");
        } else {
            println!("pending review {id}: {}", s(&r["body"]));
        }
    }
    Ok(())
}

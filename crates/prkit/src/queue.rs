//! The PRs waiting on my review: requested, plus re-reviews (I reviewed without
//! approving, I'm no longer requested, and commits landed after my review).

use crate::gh::{days_since, gh, graphql, me, s};
use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};

const Q: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 50) {
    nodes { ... on PullRequest {
      number title url isDraft createdAt updatedAt
      additions deletions changedFiles
      baseRefName headRefName
      repository { nameWithOwner defaultBranchRef { name } }
      author { login }
      reviewDecision
      reviewRequests(first: 20) {
        nodes { requestedReviewer { ... on User { login } ... on Team { name } } }
      }
      latestReviews(first: 20) { nodes { author { login } state submittedAt } }
      comments { totalCount }
      reviewThreads(first: 100) { nodes { isResolved } }
      commits(last: 1) { nodes { commit { committedDate statusCheckRollup { state } } } }
    } }
  }
}"#;

fn search(q: &str) -> Result<Vec<Value>> {
    let d = graphql(Q, json!({ "q": q }), 3, 2).map_err(|e| anyhow::anyhow!("GitHub search failed: {q}: {e}"))?;
    Ok(d["search"]["nodes"].as_array().cloned().unwrap_or_default())
}

fn my_review<'a>(pr: &'a Value, me: &str) -> Option<&'a Value> {
    pr["latestReviews"]["nodes"].as_array()?.iter().find(|r| r["author"]["login"] == me)
}

fn summarize(pr: &Value, me: &str, why: &str) -> Value {
    let mine = my_review(pr, me);
    let bot = Regex::new(r"coderabbit|copilot|\[bot\]$").unwrap();
    let commit = &pr["commits"]["nodes"][0]["commit"];
    let human: Vec<String> = pr["latestReviews"]["nodes"].as_array().into_iter().flatten()
        .filter(|r| r["author"]["login"] != me && !bot.is_match(s(&r["author"]["login"])))
        .map(|r| format!("{}:{}", s(&r["author"]["login"]), s(&r["state"]))).collect();
    let others: Vec<String> = pr["reviewRequests"]["nodes"].as_array().into_iter().flatten()
        .filter_map(|n| n["requestedReviewer"]["login"].as_str().or(n["requestedReviewer"]["name"].as_str()))
        .filter(|l| *l != me).map(String::from).collect();
    let open_threads = pr["reviewThreads"]["nodes"].as_array().into_iter().flatten().filter(|t| t["isResolved"] != true).count();
    json!({
        "repo": pr["repository"]["nameWithOwner"],
        "number": pr["number"], "title": pr["title"], "url": pr["url"], "why": why,
        "author": pr["author"]["login"],
        "createdAt": pr["createdAt"], "updatedAt": pr["updatedAt"],
        "ageDays": days_since(s(&pr["createdAt"])),
        "idleDays": days_since(s(&pr["updatedAt"])),
        "additions": pr["additions"], "deletions": pr["deletions"], "changedFiles": pr["changedFiles"],
        "base": pr["baseRefName"], "head": pr["headRefName"],
        "stacked": pr["baseRefName"] != pr["repository"]["defaultBranchRef"]["name"],
        "ci": commit["statusCheckRollup"]["state"].as_str().unwrap_or("NONE"),
        "lastCommit": commit["committedDate"],
        "decision": pr["reviewDecision"],
        "myLastReview": mine.map(|r| format!("{} {}", s(&r["state"]), s(&r["submittedAt"]))),
        "humanReviews": human,
        "otherRequested": others,
        "comments": pr["comments"]["totalCount"],
        "openThreads": open_threads,
    })
}

pub fn run(org: &str, as_json: bool) -> Result<()> {
    let me = me()?;
    let requested = search(&format!("org:{org} is:pr is:open draft:false review-requested:{me}"))?;
    let reviewed = search(&format!("org:{org} is:pr is:open draft:false reviewed-by:{me} -author:{me}"))?;

    let urls: Vec<&Value> = requested.iter().map(|p| &p["url"]).collect();
    let mut prs: Vec<Value> = requested.iter().map(|p| summarize(p, &me, "requested")).collect();
    for p in &reviewed {
        if urls.contains(&&p["url"]) {
            continue;
        }
        let mine = my_review(p, &me);
        if mine.is_some_and(|r| r["state"] == "APPROVED") {
            continue;
        }
        // No review of mine, or no commit since it: nothing to re-review.
        let since = mine.and_then(|r| r["submittedAt"].as_str()).unwrap_or("9");
        if s(&p["commits"]["nodes"][0]["commit"]["committedDate"]) <= since {
            continue;
        }
        prs.push(summarize(p, &me, "re-review"));
    }
    prs.sort_by(|a, b| s(&a["createdAt"]).cmp(s(&b["createdAt"])));

    // For stacked PRs, find the PR that owns the base branch.
    for pr in prs.iter_mut() {
        let mut parent = Value::Null;
        if pr["stacked"] == true {
            let out = gh(&["pr", "list", "-R", s(&pr["repo"]), "--head", s(&pr["base"]), "--state", "all", "--limit", "1",
                "--json", "number,state,reviewDecision", "--jq", ".[0] // empty"], None).unwrap_or_default();
            parent = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        }
        pr["parent"] = parent;
    }
    // A PR other queued PRs sit on blocks them.
    let snapshot = prs.clone();
    for pr in prs.iter_mut() {
        let blocks: Vec<Value> = snapshot.iter()
            .filter(|o| o["parent"]["number"] == pr["number"] && o["repo"] == pr["repo"])
            .map(|o| o["number"].clone()).collect();
        pr["blocks"] = json!(blocks);
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&prs)?);
        return Ok(());
    }
    println!("# Review queue for {me}: {} PR(s)\n", prs.len());
    let join = |v: &Value, empty: &str| -> String {
        let a: Vec<&str> = v.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if a.is_empty() { empty.to_string() } else { a.join(", ") }
    };
    for p in &prs {
        println!("## {}#{} {}", s(&p["repo"]), p["number"], s(&p["title"]));
        println!("{}", s(&p["url"]));
        println!("- {} · by {} · +{}/-{}, {} files · opened {}d ago, idle {}d", s(&p["why"]), s(&p["author"]),
            p["additions"], p["deletions"], p["changedFiles"], p["ageDays"], p["idleDays"]);
        println!("- CI: {} · decision: {} · comments: {} · open threads: {}", s(&p["ci"]),
            p["decision"].as_str().unwrap_or("none"), p["comments"], p["openThreads"]);
        println!("- other reviews: {} · also requested: {}", join(&p["humanReviews"], "none"), join(&p["otherRequested"], "nobody"));
        if let Some(m) = p["myLastReview"].as_str() {
            println!("- my last review: {m} · last commit {}", s(&p["lastCommit"]));
        }
        if p["stacked"] == true {
            let par = &p["parent"];
            let detail = if par.is_null() { String::new() } else {
                let dec = par["reviewDecision"].as_str().filter(|d| !d.is_empty()).unwrap_or("no decision");
                format!(" (#{}, {}, {dec})", par["number"], s(&par["state"]))
            };
            println!("- stacked on `{}`{detail}", s(&p["base"]));
        }
        let blocks: Vec<String> = p["blocks"].as_array().into_iter().flatten().map(|n| format!("#{n}")).collect();
        if !blocks.is_empty() {
            println!("- blocks: {}", blocks.join(", "));
        }
        println!();
    }
    Ok(())
}

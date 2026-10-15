//! Rank a team's open PRs by who is blocked. Fetches every open, non-draft PR in the
//! org (cached 15 min) and the team's members (cached 24 h) into $TMPDIR, then keeps
//! PRs a teammate authored or is a requested reviewer on.

use crate::gh::{days_since, gh, graphql, me, s};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const Q: &str = r#"
query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 25, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest {
      number title url isDraft createdAt updatedAt
      additions deletions changedFiles
      repository { name }
      author { login }
      reviewDecision mergeable
      reviewRequests(first: 20) {
        nodes { requestedReviewer { ... on User { login } ... on Team { name } } }
      }
      reviews(first: 30) { nodes { author { login } state submittedAt } }
      comments { totalCount }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      labels(first: 10) { nodes { name } }
    } }
  }
}"#;

pub const DEFAULT_BOTS: [&str; 6] = ["coderabbitai", "copilot", "dependabot", "github-actions", "renovate", "sonarcloud"];

pub struct Opts {
    pub org: String,
    pub team: Option<String>,
    pub window: i64,
    pub nudge: i64,
    pub fresh: bool,
    pub bots: Vec<String>,
}

fn tmp() -> PathBuf {
    PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()))
}

fn fresh_enough(p: &Path, max: u64) -> bool {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age <= Duration::from_secs(max))
}

fn fetch_prs(org: &str, out: &Path) -> Result<()> {
    let mut all: Vec<Value> = Vec::new();
    let mut after = Value::Null;
    loop {
        let d = graphql(Q, json!({ "q": format!("org:{org} is:pr is:open draft:false"), "after": after }), 4, 3)?;
        all.extend(d["search"]["nodes"].as_array().cloned().unwrap_or_default());
        if d["search"]["pageInfo"]["hasNextPage"] != true {
            break;
        }
        after = d["search"]["pageInfo"]["endCursor"].clone();
    }
    std::fs::write(out, serde_json::to_string(&all)?)?;
    eprintln!("{} open PRs -> {}", all.len(), out.display());
    Ok(())
}

fn fetch_team(org: &str, team: &str, out: &Path) -> Result<()> {
    if fresh_enough(out, 86400) {
        return Ok(());
    }
    match gh(&["api", &format!("orgs/{org}/teams/{team}/members"), "--paginate", "--jq", ".[].login"], None) {
        Ok(logins) => {
            let v: Vec<&str> = logins.lines().filter(|l| !l.is_empty()).collect();
            std::fs::write(out, serde_json::to_string(&v)?)?;
            eprintln!("team {team}: {} members", v.len());
            Ok(())
        }
        Err(_) => {
            let teams = gh(&["api", &format!("orgs/{org}/teams"), "--paginate", "--jq", ".[] | \"    \" + .slug"], None).unwrap_or_default();
            bail!("cannot read team '{team}' in org '{org}' (needs read:org scope)\n  available teams:\n{teams}");
        }
    }
}

struct Pr {
    repo: String,
    num: i64,
    title: String,
    url: String,
    author: String,
    draft: bool,
    dec: String,
    reqd: Vec<String>,
    humanrev: Vec<String>,
    idle: i64,
    size: i64,
    ci: String,
    bot_author: bool,
    mine: bool,
    owed: bool,
    bucket: &'static str,
}

impl Pr {
    fn cimark(&self) -> &'static str {
        match self.ci.as_str() {
            "SUCCESS" => "ci:pass",
            "FAILURE" | "ERROR" => "**ci:FAIL**",
            "PENDING" => "ci:running",
            _ => "ci:none",
        }
    }
}

pub fn run(o: Opts) -> Result<()> {
    let cache = tmp().join(format!("pr-triage-{}.json", o.org));
    let teamfile = tmp().join(format!("pr-triage-team-{}-{}.json", o.org, o.team.as_deref().unwrap_or("all")));
    if o.fresh || !fresh_enough(&cache, 900) {
        fetch_prs(&o.org, &cache)?;
    } else {
        eprintln!("using cache {} (< 15 min old; --fresh to refetch)", cache.display());
    }
    let (team, teamname): (Vec<String>, String) = match &o.team {
        Some(t) => {
            if o.fresh {
                let _ = std::fs::remove_file(&teamfile);
            }
            fetch_team(&o.org, t, &teamfile)?;
            (serde_json::from_str(&std::fs::read_to_string(&teamfile)?)?, t.clone())
        }
        None => {
            std::fs::write(&teamfile, "[]")?;
            (Vec::new(), format!("{} (whole org)", o.org))
        }
    };
    let me = me()?;
    let raw: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&cache)?)?;

    let onteam = |l: &str| team.is_empty() || team.iter().any(|m| m == l);
    let isbot = |l: &str| {
        let d = l.to_lowercase();
        o.bots.iter().any(|b| d.starts_with(b.as_str())) || d.contains("[bot]")
    };
    let trunc = |t: &str| {
        let c: Vec<char> = t.chars().collect();
        if c.len() > 64 { format!("{}...", c[..61].iter().collect::<String>()) } else { t.to_string() }
    };

    let all: Vec<Pr> = raw.iter().map(|p| {
        let author = p["author"]["login"].as_str().unwrap_or("ghost").to_string();
        let reqd: Vec<String> = p["reviewRequests"]["nodes"].as_array().into_iter().flatten()
            .filter_map(|n| n["requestedReviewer"]["login"].as_str().or(n["requestedReviewer"]["name"].as_str()))
            .map(String::from).collect();
        let mut humanrev: Vec<String> = p["reviews"]["nodes"].as_array().into_iter().flatten()
            .filter(|r| !r["author"].is_null() && !isbot(s(&r["author"]["login"])))
            .map(|r| s(&r["author"]["login"]).to_string()).collect();
        humanrev.sort();
        humanrev.dedup();
        let mut pr = Pr {
            repo: s(&p["repository"]["name"]).into(),
            num: p["number"].as_i64().unwrap_or(0),
            title: trunc(s(&p["title"])),
            url: s(&p["url"]).into(),
            draft: p["isDraft"] == true,
            dec: p["reviewDecision"].as_str().unwrap_or("NONE").into(),
            idle: days_since(s(&p["updatedAt"])),
            size: p["additions"].as_i64().unwrap_or(0) + p["deletions"].as_i64().unwrap_or(0),
            ci: p["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str().unwrap_or("NONE").into(),
            bot_author: isbot(&author),
            mine: onteam(&author),
            owed: reqd.iter().any(|r| onteam(r)),
            author,
            reqd,
            humanrev,
            bucket: "",
        };
        pr.bucket = match () {
            _ if pr.dec == "APPROVED" => "merge",
            _ if pr.dec == "CHANGES_REQUESTED" => "author",
            _ if matches!(pr.ci.as_str(), "FAILURE" | "ERROR") => "ci",
            _ if pr.reqd.is_empty() && pr.humanrev.is_empty() => "unowned",
            _ if pr.humanrev.is_empty() => "waiting",
            _ => "inreview",
        };
        pr
    }).collect();

    let scoped: Vec<&Pr> = all.iter().filter(|p| p.mine || p.owed).collect();
    let human: Vec<&Pr> = scoped.iter().copied().filter(|p| !p.draft && !p.bot_author).collect();
    let act: Vec<&Pr> = human.iter().copied().filter(|p| p.idle <= o.window).collect();
    let stale: Vec<&Pr> = human.iter().copied().filter(|p| p.idle > o.window).collect();
    let bots: Vec<&Pr> = scoped.iter().copied().filter(|p| p.bot_author && !p.draft).collect();
    let own: Vec<&Pr> = act.iter().copied().filter(|p| p.mine).collect();
    let external: Vec<&Pr> = act.iter().copied().filter(|p| !p.mine).collect();

    let revmark = |p: &Pr| {
        if p.reqd.is_empty() { "reviewers: none".to_string() } else {
            "reviewers: ".to_string() + &p.reqd.iter().map(|r| format!("@{r}{}", if onteam(r) { "" } else { "\\*" })).collect::<Vec<_>>().join(", ")
        }
    };
    let row = |p: &Pr| format!("- **{}d idle** · [{}#{}]({}) · @{} · {}L · {}\n  {} — {}",
        p.idle, p.repo, p.num, p.url, p.author, p.size, p.cimark(), p.title, revmark(p));
    let by_idle = |v: &mut Vec<&Pr>| v.sort_by_key(|p| -p.idle);

    let mut out: Vec<String> = vec![
        format!("# PR triage — {teamname}"),
        String::new(),
        format!("{} active (touched ≤ {}d) · {} stale · {} bot", act.len(), o.window, stale.len(), bots.len()),
        if team.is_empty() { "_Scope: whole org._".into() } else {
            format!("_Scope: {} members of **{teamname}** — PRs they authored, plus PRs where they are a requested reviewer. {} other org PRs hidden._", team.len(), all.len() - scoped.len())
        },
    ];
    let sections = [
        ("Needs a reviewer", "unowned", "Nobody is assigned and no human has reviewed. These are invisible — assign someone."),
        ("Waiting on an assigned reviewer", "waiting", "Reviewer is named but has not reviewed yet. Nudge the named reviewer."),
        ("Ball with the author — changes requested", "author", "Review landed; author has not responded yet. Check whether they are stuck."),
        ("Ball with the author — CI failing", "ci", "Not reviewable until CI is green. Author action, not a review bottleneck."),
        ("Approved — not merged", "merge", "Cleared review. Merge or nudge the author to merge."),
        ("In review — conversation ongoing", "inreview", "A human has engaged. Usually healthy; watch the idle time."),
    ];
    for (title, b, why) in sections {
        let mut v: Vec<&Pr> = own.iter().copied().filter(|p| p.bucket == b).collect();
        if !v.is_empty() {
            by_idle(&mut v);
            out.push(format!("\n## {title} — {}\n_{why}_\n\n{}", v.len(), v.iter().map(|p| row(p)).collect::<Vec<_>>().join("\n")));
        }
    }

    // Who is blocked (teammates only).
    let mut blocked: BTreeMap<&str, (usize, i64, i64)> = BTreeMap::new();
    for p in own.iter().filter(|p| matches!(p.bucket, "unowned" | "waiting") && p.idle >= o.nudge) {
        let e = blocked.entry(&p.author).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 = e.1.max(p.idle);
        e.2 += p.size;
    }
    if blocked.is_empty() {
        out.push(format!("\n## Who is blocked\n\nNobody on {teamname} has been waiting {}d+ on review. The active queue is healthy.", o.nudge));
    } else {
        let mut v: Vec<_> = blocked.into_iter().collect();
        v.sort_by_key(|(_, (n, worst, _))| (-worst, -(*n as i64)));
        out.push(format!("\n## Who is blocked\n_Teammates waiting {}d+ on review, longest wait first. Start at the top._\n\n{}", o.nudge,
            v.iter().map(|(a, (n, worst, l))| format!("- **@{a}** — waiting {worst}d, {n} PR(s), {l}L to review")).collect::<Vec<_>>().join("\n")));
    }

    let mut ext = external.clone();
    if !ext.is_empty() {
        by_idle(&mut ext);
        out.push(format!("\n## Review the team owes — {}\n_Authored outside {teamname}, but a teammate is the requested reviewer._\n\n{}",
            ext.len(), ext.iter().map(|p| row(p)).collect::<Vec<_>>().join("\n")));
    }

    // Reviewer load.
    let mut load: BTreeMap<&str, (usize, i64)> = BTreeMap::new();
    for p in act.iter().filter(|p| matches!(p.bucket, "waiting" | "inreview")) {
        for r in &p.reqd {
            let e = load.entry(r).or_insert((0, 0));
            e.0 += 1;
            e.1 = e.1.max(p.idle);
        }
    }
    if !load.is_empty() {
        let mut v: Vec<_> = load.into_iter().collect();
        v.sort_by_key(|(_, (n, worst))| (-(*n as i64), -worst));
        out.push(format!("\n## Reviewer load\n_Outstanding review requests. \\* = outside {teamname}._\n\n{}",
            v.iter().map(|(r, (n, worst))| format!("- **@{r}**{} — {n} pending, oldest {worst}d", if onteam(r) { "" } else { "\\*" })).collect::<Vec<_>>().join("\n")));
    }

    let mut mine: Vec<&Pr> = act.iter().copied().filter(|p| p.reqd.contains(&me)).collect();
    if mine.is_empty() {
        out.push(format!("\n## Your queue\n\nNothing is waiting on @{me}."));
    } else {
        by_idle(&mut mine);
        out.push(format!("\n## Your queue — {}\n_PRs where @{me} is a requested reviewer._\n\n{}", mine.len(), mine.iter().map(|p| row(p)).collect::<Vec<_>>().join("\n")));
    }

    let mut st: BTreeMap<&str, (usize, i64)> = BTreeMap::new();
    for p in &stale {
        let e = st.entry(&p.author).or_insert((0, 0));
        e.0 += 1;
        e.1 = e.1.max(p.idle);
    }
    if !st.is_empty() {
        let mut v: Vec<_> = st.into_iter().collect();
        v.sort_by_key(|(_, (n, _))| -(*n as i64));
        out.push(format!("\n## Stale — untouched > {}d\n_Close or revive; they distort every other count._\n\n{}", o.window,
            v.iter().map(|(a, (n, worst))| format!("- @{a} — {n} PR(s), oldest {worst}d idle")).collect::<Vec<_>>().join("\n")));
    }

    if !bots.is_empty() {
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for p in &bots {
            *per.entry(&p.repo).or_default() += 1;
        }
        out.push(format!("\n## Bot PRs — {}\n_Dependency/automation PRs, excluded from the rankings above._\n\n{}", bots.len(),
            per.iter().map(|(r, n)| format!("- {r} — {n}")).collect::<Vec<_>>().join("\n")));
    }

    println!("{}", out.join("\n"));
    println!("\n_org:{} · team:{} · active≤{}d · blocked≥{}d · data: {}_", o.org, o.team.as_deref().unwrap_or("all"), o.window, o.nudge, cache.display());
    Ok(())
}

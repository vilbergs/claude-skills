//! prkit — GitHub pull request helpers on top of the `gh` CLI.

mod gh;
mod queue;
mod review;
mod triage;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "prkit", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// A PR's review summaries, conversation and inline threads (with comment ids)
    Threads {
        /// PR number or URL
        pr: String,
        #[arg(short = 'R', long)]
        repo: Option<String>,
        /// Only unresolved inline threads
        #[arg(long)]
        unresolved: bool,
        /// Show bot comments (CodeRabbit, Copilot...) in full
        #[arg(long)]
        bots: bool,
        #[arg(long)]
        json: bool,
    },
    /// Post one review from a draft dir (summary.md, anchors.tsv, <id>.md); anchors are checked against the diff
    Review {
        pr: String,
        #[arg(short = 'R', long)]
        repo: Option<String>,
        #[arg(long)]
        dir: PathBuf,
        /// COMMENT, APPROVE or REQUEST_CHANGES
        #[arg(long, default_value = "COMMENT")]
        event: String,
        /// Validate and print the payload without posting
        #[arg(long)]
        dry_run: bool,
    },
    /// Reply to an inline review comment. BODY is text, @file or -
    Reply {
        pr: String,
        comment_id: u64,
        body: String,
        #[arg(short = 'R', long)]
        repo: Option<String>,
    },
    /// Show (or --delete) my pending, unsubmitted review on a PR
    Pending {
        pr: String,
        #[arg(short = 'R', long)]
        repo: Option<String>,
        #[arg(long)]
        delete: bool,
    },
    /// Lines of each file a review comment can anchor to (new-file side)
    DiffLines {
        pr: String,
        #[arg(short = 'R', long)]
        repo: Option<String>,
    },
    /// PRs waiting on my review across the org (requested + re-reviews)
    Queue {
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Rank a team's open PRs by who is blocked
    Triage {
        /// GitHub team slug (default from config)
        #[arg(long)]
        team: Option<String>,
        /// Whole org, no team filter
        #[arg(long, conflicts_with = "team")]
        all: bool,
        #[arg(long)]
        org: Option<String>,
        /// A PR is "active" if touched within this many days
        #[arg(long, default_value_t = 21)]
        window: i64,
        /// An author counts as blocked after this many idle days
        #[arg(long, default_value_t = 2)]
        nudge: i64,
        /// Refetch instead of reusing the caches
        #[arg(long)]
        fresh: bool,
    },
}

fn main() {
    common::reset_sigpipe();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("prkit: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let cfg = common::load()?.github;
    let org = |o: Option<String>| o.or(cfg.org.clone()).context("no org: pass --org or set [github] org in the config");
    match cli.cmd {
        Cmd::Threads { pr, repo, unresolved, bots, json } => {
            let (repo, n) = gh::pr_ref(&pr, repo.as_deref())?;
            review::threads(&repo, n, unresolved, json, bots)
        }
        Cmd::Review { pr, repo, dir, event, dry_run } => {
            let (repo, n) = gh::pr_ref(&pr, repo.as_deref())?;
            review::submit(&repo, n, &dir, &event, dry_run)
        }
        Cmd::Reply { pr, comment_id, body, repo } => {
            let (repo, n) = gh::pr_ref(&pr, repo.as_deref())?;
            review::reply(&repo, n, comment_id, &common::read_arg(&body)?)
        }
        Cmd::Pending { pr, repo, delete } => {
            let (repo, n) = gh::pr_ref(&pr, repo.as_deref())?;
            review::pending(&repo, n, delete)
        }
        Cmd::DiffLines { pr, repo } => {
            let (repo, n) = gh::pr_ref(&pr, repo.as_deref())?;
            for (file, lines) in review::diff_lines(&repo, n)? {
                println!("{file}\t{}", ranges(&lines.into_iter().collect::<Vec<_>>()));
            }
            Ok(())
        }
        Cmd::Queue { org: o, json } => queue::run(&org(o)?, json),
        Cmd::Triage { team, all, org: o, window, nudge, fresh } => triage::run(triage::Opts {
            org: org(o)?,
            team: if all { None } else { team.or(cfg.team.clone()) },
            window,
            nudge,
            fresh,
            bots: cfg.bots.clone().unwrap_or_else(|| triage::DEFAULT_BOTS.iter().map(|s| s.to_string()).collect()),
        }),
    }
}

/// [1,2,3,7,8] -> "1-3,7-8"
fn ranges(v: &[u64]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        while i + 1 < v.len() && v[i + 1] == v[i] + 1 {
            i += 1;
        }
        out.push(if v[i] == start { start.to_string() } else { format!("{start}-{}", v[i]) });
        i += 1;
    }
    out.join(",")
}

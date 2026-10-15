//! asc — App Store Connect API from the terminal.
//!
//! Auth: an API key (`AuthKey_<KEYID>.p8`) and its issuer id. Key id and issuer come
//! from flags, ASC_KEY_ID / ASC_ISSUER_ID, or `[asc]` in the config. The .p8 file is
//! looked up in the configured key_dir, ~/.appstoreconnect/private_keys, ~/.private_keys
//! and ~/private_keys.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const API: &str = "https://api.appstoreconnect.apple.com";

#[derive(Parser)]
#[command(name = "asc", version, about)]
struct Cli {
    /// API key id (the KEYID in AuthKey_KEYID.p8)
    #[arg(long, global = true)]
    key: Option<String>,
    #[arg(long, global = true)]
    issuer: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// GET a path like /v1/apps; prints JSON
    Get {
        path: String,
        /// Follow links.next and merge `data` from every page
        #[arg(long)]
        all: bool,
    },
    /// POST a JSON body (text, @file or -)
    Post { path: String, body: String },
    /// PATCH a JSON body (text, @file or -)
    Patch { path: String, body: String },
    Delete { path: String },
    /// Apps: id, bundle id, name
    Apps,
    /// Xcode Cloud products and their workflows
    Workflows,
    /// Recent build runs of a workflow
    Runs {
        workflow: String,
        #[arg(long, default_value_t = 5)]
        limit: u32,
    },
    /// A build run's actions with their result, issues and artifacts
    Run {
        id: String,
        /// Print artifact download URLs (long, signed, short-lived)
        #[arg(long)]
        urls: bool,
    },
    /// Builds of an app (TestFlight/App Store), newest first
    Builds {
        app: String,
        /// Filter by build number
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
}

struct Asc {
    jwt: String,
}

impl Asc {
    fn new(key: Option<String>, issuer: Option<String>) -> Result<Self> {
        let cfg = common::load()?.asc;
        let issuer = issuer.or(std::env::var("ASC_ISSUER_ID").ok()).or(cfg.issuer_id)
            .context("no issuer id: pass --issuer, set ASC_ISSUER_ID or [asc] issuer_id in the config")?;
        let mut dirs: Vec<PathBuf> = cfg.key_dir.iter().map(|d| common::expand(d)).collect();
        dirs.extend(["~/.appstoreconnect/private_keys", "~/.private_keys", "~/private_keys"].map(common::expand));
        let key_id = match key.or(std::env::var("ASC_KEY_ID").ok()).or(cfg.key_id) {
            Some(k) => k,
            None => {
                // Exactly one key around: use it.
                let found: Vec<String> = dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten()
                    .filter_map(|e| e.file_name().to_str()?.strip_prefix("AuthKey_")?.strip_suffix(".p8").map(String::from))
                    .collect();
                match found.as_slice() {
                    [one] => one.clone(),
                    [] => bail!("no key id and no AuthKey_*.p8 found"),
                    many => bail!("several keys found ({}); pass --key", many.join(", ")),
                }
            }
        };
        let file = format!("AuthKey_{key_id}.p8");
        let path = dirs.iter().map(|d| d.join(&file)).find(|p| p.exists())
            .ok_or_else(|| anyhow!("{file} not found in {}", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))?;
        let pem = std::fs::read(&path)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(key_id);
        let claims = json!({ "iss": issuer, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1" });
        let jwt = encode(&header, &claims, &EncodingKey::from_ec_pem(&pem).context("reading the .p8 key")?)?;
        Ok(Self { jwt })
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{API}{}", if path.starts_with('/') { path.to_string() } else { format!("/{path}") }) };
        let req = ureq::request(method, &url).set("Authorization", &format!("Bearer {}", self.jwt)).set("Content-Type", "application/json");
        let res = match body { Some(b) => req.send_json(b), None => req.call() };
        match res {
            Ok(r) => {
                let text = r.into_string()?;
                Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text)? })
            }
            Err(ureq::Error::Status(code, r)) => {
                let v: Value = r.into_json().unwrap_or(Value::Null);
                let errs: Vec<String> = v["errors"].as_array().into_iter().flatten()
                    .map(|e| format!("{}: {}", e["title"].as_str().unwrap_or(""), e["detail"].as_str().unwrap_or(""))).collect();
                bail!("HTTP {code} {}", errs.join("; "))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn get_all(&self, path: &str) -> Result<Value> {
        let mut first = self.call("GET", path, None)?;
        let mut next = first["links"]["next"].as_str().map(String::from);
        while let Some(url) = next {
            let page = self.call("GET", &url, None)?;
            if let (Some(acc), Some(more)) = (first["data"].as_array_mut(), page["data"].as_array()) {
                acc.extend(more.iter().cloned());
            }
            next = page["links"]["next"].as_str().map(String::from);
        }
        first["links"] = Value::Null;
        Ok(first)
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("-")
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn body(arg: &str) -> Result<Value> {
    serde_json::from_str(&common::read_arg(arg)?).context("body is not valid JSON")
}

fn main() {
    common::reset_sigpipe();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("asc: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let asc = Asc::new(cli.key, cli.issuer)?;
    let pretty = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap());
    match cli.cmd {
        Cmd::Get { path, all } => pretty(&if all { asc.get_all(&path)? } else { asc.call("GET", &path, None)? }),
        Cmd::Post { path, body: b } => pretty(&asc.call("POST", &path, Some(&body(&b)?))?),
        Cmd::Patch { path, body: b } => pretty(&asc.call("PATCH", &path, Some(&body(&b)?))?),
        Cmd::Delete { path } => pretty(&asc.call("DELETE", &path, None)?),
        Cmd::Apps => {
            for a in asc.get_all("/v1/apps?fields[apps]=name,bundleId&limit=200")?["data"].as_array().into_iter().flatten() {
                println!("{}\t{}\t{}", s(&a["id"]), s(&a["attributes"]["bundleId"]), s(&a["attributes"]["name"]));
            }
        }
        Cmd::Workflows => {
            for p in asc.get_all("/v1/ciProducts?include=app&limit=200")?["data"].as_array().into_iter().flatten() {
                println!("{}  {} (product {})", s(&p["attributes"]["productType"]), s(&p["attributes"]["name"]), s(&p["id"]));
                let wf = asc.get_all(&format!("/v1/ciProducts/{}/workflows?limit=200", s(&p["id"])))?;
                for w in wf["data"].as_array().into_iter().flatten() {
                    let a = &w["attributes"];
                    println!("  {}\t{}{}", s(&w["id"]), s(&a["name"]), if a["isEnabled"] == false { " (disabled)" } else { "" });
                }
            }
        }
        Cmd::Runs { workflow, limit } => {
            let r = asc.call("GET", &format!("/v1/ciWorkflows/{workflow}/buildRuns?limit={limit}&sort=-number"), None)?;
            for run in r["data"].as_array().into_iter().flatten() {
                let a = &run["attributes"];
                let src = &a["sourceCommit"];
                println!("#{} {}\t{} {}\tstarted {}\tfinished {}\t{} {}", a["number"], s(&run["id"]), s(&a["executionProgress"]), s(&a["completionStatus"]),
                    s(&a["startedDate"]), s(&a["finishedDate"]), s(&src["commitSha"]).chars().take(8).collect::<String>(),
                    s(&src["message"]).lines().next().unwrap_or(""));
            }
        }
        Cmd::Run { id, urls } => {
            let run = asc.call("GET", &format!("/v1/ciBuildRuns/{id}"), None)?;
            let a = &run["data"]["attributes"];
            println!("build run #{} {}: {} {}", a["number"], id, s(&a["executionProgress"]), s(&a["completionStatus"]));
            for act in asc.get_all(&format!("/v1/ciBuildRuns/{id}/actions"))?["data"].as_array().into_iter().flatten() {
                let aa = &act["attributes"];
                let issues = &aa["issueCounts"];
                println!("- {} [{}] {} {} (errors {}, test failures {}, warnings {})", s(&aa["name"]), s(&aa["actionType"]),
                    s(&aa["executionProgress"]), s(&aa["completionStatus"]), n(&issues["errors"]), n(&issues["testFailures"]), n(&issues["warnings"]));
                let arts = asc.get_all(&format!("/v1/ciBuildActions/{}/artifacts", s(&act["id"])))?;
                for art in arts["data"].as_array().into_iter().flatten() {
                    let x = &art["attributes"];
                    println!("    {} ({}, {} bytes)", s(&x["fileName"]), s(&x["fileType"]), x["fileSize"]);
                    if urls {
                        println!("      {}", s(&x["downloadUrl"]));
                    }
                }
            }
        }
        Cmd::Builds { app, version, limit } => {
            let mut path = format!("/v1/builds?filter[app]={app}&sort=-uploadedDate&limit={limit}");
            if let Some(v) = version {
                path += &format!("&filter[version]={v}");
            }
            for b in asc.call("GET", &path, None)?["data"].as_array().into_iter().flatten() {
                let a = &b["attributes"];
                println!("{}\tbuild {}\t{}\tuploaded {}\texpired {}", s(&b["id"]), s(&a["version"]), s(&a["processingState"]), s(&a["uploadedDate"]), a["expired"]);
            }
        }
    }
    Ok(())
}

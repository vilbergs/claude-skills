//! Shared config loading for the claude-skills binaries.
//!
//! Everything account-specific (database ids, org names, key ids) lives in a local
//! TOML file, never in this repo: `$CLAUDE_SKILLS_CONFIG`, else
//! `~/.config/claude-skills/config.toml`. A missing file means all defaults.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub notion: NotionConfig,
    pub github: GithubConfig,
    pub asc: AscConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotionConfig {
    /// File holding the integration secret. Default `~/.notion_token`.
    pub token_file: Option<String>,
    pub issue: Option<IssueConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct IssueConfig {
    pub database_id: String,
    pub data_source_id: String,
    /// Notion user id written to the reporter property on every new issue.
    pub reporter_id: Option<String>,
    pub reporter_name: Option<String>,
    /// alias -> template. The alias is what `--template` accepts.
    pub templates: BTreeMap<String, TemplateConfig>,
    /// flag name -> property name, overriding the defaults (e.g. project = "🛠️ Project").
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TemplateConfig {
    /// Template name as it appears in Notion.
    pub name: String,
    /// Value of the Type property this template implies; none means `--type` is required.
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GithubConfig {
    pub org: Option<String>,
    pub team: Option<String>,
    /// Login prefixes treated as bots. Defaults to common review/dependency bots.
    pub bots: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AscConfig {
    pub issuer_id: Option<String>,
    pub key_id: Option<String>,
    /// Extra directory to search for `AuthKey_<id>.p8`.
    pub key_dir: Option<String>,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

/// Expand a leading `~/`.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

pub fn config_path() -> PathBuf {
    match std::env::var("CLAUDE_SKILLS_CONFIG") {
        Ok(p) if !p.is_empty() => expand(&p),
        _ => home().join(".config/claude-skills/config.toml"),
    }
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Read an argument that may be inline text, `-` for stdin, or `@path` for a file.
pub fn read_arg(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        Ok(s)
    } else if let Some(p) = arg.strip_prefix('@') {
        std::fs::read_to_string(expand(p)).with_context(|| format!("reading {p}"))
    } else {
        Ok(arg.to_string())
    }
}

/// Exit quietly when stdout is closed early (e.g. piped into `head`) instead of panicking.
pub fn reset_sigpipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

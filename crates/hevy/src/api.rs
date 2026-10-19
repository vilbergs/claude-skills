use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, SystemTime};

const API: &str = "https://api.hevyapp.com";
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// A non-2xx answer, kept typed so callers can react to a status (409 on body measurements).
#[derive(Debug)]
pub struct HttpError {
    pub code: u16,
    pub message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let hint = match self.code {
            401 => " (check the API key; the API needs Hevy Pro)",
            404 => " (no such id or path)",
            _ => "",
        };
        write!(f, "HTTP {}: {}{hint}", self.code, self.message)
    }
}

impl std::error::Error for HttpError {}

pub struct Hevy {
    key: String,
}

impl Hevy {
    pub fn new() -> Result<Self> {
        if let Ok(k) = std::env::var("HEVY_API_KEY")
            && !k.trim().is_empty()
        {
            return Ok(Self { key: k.trim().to_string() });
        }
        let cfg = common::load()?.hevy;
        let path = common::expand(cfg.api_key_file.as_deref().unwrap_or("~/.hevy_api_key"));
        let key = std::fs::read_to_string(&path).map(|s| s.trim().to_string()).map_err(|_| {
            anyhow!("no Hevy API key: put it in {} or set HEVY_API_KEY. Get one (Hevy Pro) at https://hevy.com/settings?developer", path.display())
        })?;
        Ok(Self { key })
    }

    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        // HEVY_API_URL points the client at another server (a local mock in testing).
        let base = std::env::var("HEVY_API_URL").unwrap_or_else(|_| API.to_string());
        let url = format!("{base}{}", if path.starts_with('/') { path.to_string() } else { format!("/{path}") });
        for attempt in 0..5 {
            let req = ureq::request(method, &url).set("api-key", &self.key).set("Accept", "application/json");
            let res = match body {
                Some(b) => req.send_json(b),
                None => req.call(),
            };
            match res {
                Ok(r) => {
                    let text = r.into_string()?;
                    return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) });
                }
                Err(ureq::Error::Status(429, r)) if attempt < 4 => {
                    sleep(Duration::from_secs(r.header("Retry-After").and_then(|s| s.parse().ok()).unwrap_or(1 + attempt)));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    let message = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or(text);
                    return Err(HttpError { code, message: message.trim().to_string() }.into());
                }
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!()
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.call("GET", path, None)
    }

    /// Items under `key` from a paged list endpoint (`path` without page params), page by
    /// page, until `more` returns false for a page or the pages run out.
    pub fn pages(&self, path: &str, key: &str, size: u32, mut more: impl FnMut(&[Value]) -> bool) -> Result<Vec<Value>> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        let mut page = 1;
        loop {
            let r = self.get(&format!("{path}{sep}page={page}&pageSize={size}"))?;
            let items = r[key].as_array().cloned().unwrap_or_default();
            let go_on = more(&items);
            all.extend(items);
            if !go_on || page >= r["page_count"].as_u64().unwrap_or(1) {
                return Ok(all);
            }
            page += 1;
        }
    }

    /// Every exercise template, from the cache when it is fresh.
    pub fn templates(&self, refresh: bool) -> Result<Vec<Value>> {
        let path = cache_path();
        let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age < CACHE_TTL);
        if !refresh && fresh
            && let Ok(v) = serde_json::from_str::<Vec<Value>>(&std::fs::read_to_string(&path)?)
        {
            return Ok(v);
        }
        let all = self.pages("/v1/exercise_templates", "exercise_templates", 100, |_| true)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string(&all)?)?;
        Ok(all)
    }
}

pub fn cache_path() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| common::home().join(".cache"));
    base.join("claude-skills/hevy-exercises.json")
}

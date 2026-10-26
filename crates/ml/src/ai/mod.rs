//! Language models for strategies, with every reply recorded.
//!
//! A strategy asks a question ([`Request`]) and gets text back. The first time, the question
//! goes to the model; the reply is stored on disk under the exact question, and every later
//! backtest asking the same thing gets the stored reply. So reruns are free, and a backtest
//! gives the same result every time it runs. Offline, only recorded replies are used.
//!
//! Models are named the way their providers name them: Claude models by id (`claude-opus-5-5`),
//! others with a provider prefix: `openai/<model>` for OpenAI or any compatible server, and
//! `ollama/<model>` for local models through Ollama.

mod anthropic;
mod cache;
mod openai;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use anyhow::Context;
use persephone_core::UnixNanos;
use serde::{Deserialize, Serialize};

pub use self::cache::Cache;

/// How long one call may take, retries aside.
const TIMEOUT: Duration = Duration::from_secs(180);
/// Output allowance when a request doesn't say; thinking counts against it on models that think.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;
/// New calls one backtest may make unless the config says otherwise.
pub const DEFAULT_MAX_NEW_CALLS: u32 = 200;

/// One question for a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    pub max_tokens: Option<u32>,
    /// How hard a model that supports effort should think.
    pub effort: Option<Effort>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    const fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A model's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// It came from the recorded replies, not the model.
    pub cached: bool,
}

/// Where models are reached and how freely: `…/persephone/ai.json`. Every field is optional;
/// the `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL` and `OPENAI_API_KEY`
/// environment variables win over the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
    /// An OAuth token (e.g. from `ant auth`), sent as a bearer token instead of an API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_base_url: Option<String>,
    /// Use recorded replies only; never call a model.
    #[serde(default)]
    pub offline: bool,
    /// New (unrecorded) calls one backtest may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_calls: Option<u32>,
}

impl Config {
    /// Reads `path` (a missing file is an empty config), then applies the environment.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but isn't valid.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let config = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} isn't valid", path.display()))?,
            Err(_) => Self::default(),
        };
        Ok(config.with_env())
    }

    /// The environment's keys and URLs over the file's.
    #[must_use]
    pub fn with_env(mut self) -> Self {
        let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
        self.anthropic_api_key = var("ANTHROPIC_API_KEY").or(self.anthropic_api_key);
        self.anthropic_auth_token = var("ANTHROPIC_AUTH_TOKEN").or(self.anthropic_auth_token);
        self.anthropic_base_url = var("ANTHROPIC_BASE_URL").or(self.anthropic_base_url);
        self.openai_api_key = var("OPENAI_API_KEY").or(self.openai_api_key);
        self
    }
}

/// `…/persephone/ai.json` and `…/persephone/ai-cache` in the platform data folder.
#[must_use]
pub fn default_paths() -> Option<(PathBuf, PathBuf)> {
    let root = dirs::data_dir()?.join("persephone");
    Some((root.join("ai.json"), root.join("ai-cache")))
}

/// Which service a model name goes to.
#[derive(Debug, PartialEq, Eq)]
enum Provider<'a> {
    Anthropic(&'a str),
    OpenAi(&'a str),
    Ollama(&'a str),
}

fn provider(model: &str) -> Result<Provider<'_>, String> {
    if model.starts_with("claude-") {
        Ok(Provider::Anthropic(model))
    } else if let Some(name) = model.strip_prefix("openai/").filter(|n| !n.is_empty()) {
        Ok(Provider::OpenAi(name))
    } else if let Some(name) = model.strip_prefix("ollama/").filter(|n| !n.is_empty()) {
        Ok(Provider::Ollama(name))
    } else {
        Err(format!(
            "unknown model `{model}`: use a Claude model id such as claude-opus-5-5, openai/<model> or ollama/<model>"
        ))
    }
}

/// Asks models, recording what they say.
#[derive(Debug)]
pub struct Ai {
    config: Config,
    cache: Cache,
    http: reqwest::blocking::Client,
    /// Model release dates, as far as known, by model name.
    releases: Mutex<HashMap<String, Option<UnixNanos>>>,
}

impl Ai {
    /// # Errors
    ///
    /// Returns an error if the HTTP client can't be built.
    pub fn new(config: Config, cache_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("persephone/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()?;
        let cache = Cache::new(cache_dir);
        let releases = Mutex::new(cache.releases());
        Ok(Self {
            config,
            cache,
            http,
            releases,
        })
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    #[must_use]
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// New calls one backtest may make.
    #[must_use]
    pub fn max_new_calls(&self) -> u32 {
        self.config.max_new_calls.unwrap_or(DEFAULT_MAX_NEW_CALLS)
    }

    /// The recorded reply to `request`, or the model's (recorded for next time) if `may_call`
    /// and the config isn't offline.
    ///
    /// # Errors
    ///
    /// Returns a message if the reply isn't recorded and calling isn't allowed, the model is
    /// unknown or unreachable, the key is missing or refused, or the model declines.
    pub fn complete(&self, request: &Request, may_call: bool) -> Result<Reply, String> {
        let provider = provider(&request.model)?;
        if let Some(text) = self.cache.get(request) {
            return Ok(Reply { text, cached: true });
        }
        if self.config.offline {
            return Err("this question has no recorded reply, and AI is set to offline".into());
        }
        if !may_call {
            return Err("this question has no recorded reply, and the backtest has used up its new AI calls".into());
        }
        let text = match provider {
            Provider::Anthropic(model) => {
                anthropic::complete(&self.http, &self.config, model, request)
            }
            Provider::OpenAi(model) => openai::complete(
                &self.http,
                self.config
                    .openai_base_url
                    .as_deref()
                    .unwrap_or(openai::OPENAI_URL),
                self.config.openai_api_key.as_deref(),
                model,
                request,
            ),
            Provider::Ollama(model) => openai::complete(
                &self.http,
                self.config
                    .ollama_base_url
                    .as_deref()
                    .unwrap_or(openai::OLLAMA_URL),
                None,
                model,
                request,
            ),
        }?;
        // A reply that can't be recorded still answers this call.
        let _ = self.cache.put(request, &text);
        Ok(Reply {
            text,
            cached: false,
        })
    }

    /// When `model` was released, if its provider says (and it was reachable once). A model
    /// can't know anything after its release, so data after that date is a fair test of it.
    #[must_use]
    pub fn release_date(&self, model: &str) -> Option<UnixNanos> {
        if let Ok(releases) = self.releases.lock()
            && let Some(known) = releases.get(model)
        {
            return *known;
        }
        if self.config.offline {
            return None;
        }
        let found = match provider(model).ok()? {
            Provider::Anthropic(id) => anthropic::release_date(&self.http, &self.config, id),
            Provider::OpenAi(id) => openai::release_date(
                &self.http,
                self.config
                    .openai_base_url
                    .as_deref()
                    .unwrap_or(openai::OPENAI_URL),
                self.config.openai_api_key.as_deref(),
                id,
            ),
            // Local models don't say when they were trained.
            Provider::Ollama(_) => None,
        };
        if let Ok(mut releases) = self.releases.lock() {
            releases.insert(model.to_owned(), found);
            let _ = self.cache.save_releases(&releases);
        }
        found
    }
}

/// Retries a request on rate limits, overloads, server errors and dropped connections, with
/// growing waits (or what the server asks for, up to 30 s).
fn send_with_retries(
    build: impl Fn() -> reqwest::blocking::RequestBuilder,
) -> Result<reqwest::blocking::Response, String> {
    const ATTEMPTS: u32 = 3;
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        let wait = match build().send() {
            Ok(response) => {
                let status = response.status().as_u16();
                if !matches!(status, 429 | 500 | 502 | 503 | 529) || attempt + 1 == ATTEMPTS {
                    return Ok(response);
                }
                let asked = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                last = format!("HTTP {status}");
                asked.map_or(Duration::from_secs(1 << attempt), |s| {
                    Duration::from_secs(s.min(30))
                })
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                last = e.to_string();
                Duration::from_secs(1 << attempt)
            }
            Err(e) => return Err(e.to_string()),
        };
        std::thread::sleep(wait);
    }
    Err(format!("gave up after {ATTEMPTS} tries: {last}"))
}

/// The date at the start of an RFC 3339 timestamp (`2026-04-01T…`), as nanoseconds at midnight
/// UTC.
fn date_nanos(timestamp: &str) -> Option<UnixNanos> {
    let date = timestamp.get(..10)?;
    let mut parts = date.split('-').map(str::parse::<u32>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let days = persephone_core::time::days_from_civil(i64::from(year), month, day);
    u64::try_from(days)
        .ok()?
        .checked_mul(persephone_core::time::NANOS_PER_DAY)
}

/// An API error body's message, or the status.
fn error_message(status: u16, body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        });
    match message {
        Some(message) => format!("HTTP {status}: {message}"),
        None => format!("HTTP {status}"),
    }
}

#[cfg(test)]
mod tests;

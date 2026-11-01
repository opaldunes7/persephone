//! Persephone Lab: everything in Persephone, as named tools with structured arguments, for agents
//! and scripts to read every data point and run every test.
//!
//! It has no rules about how research is done. Whatever an agent should or shouldn't do belongs to
//! that agent's own setup, which can decide from a call's tool and arguments before it's made. Lab
//! only keeps a log of every call. Three things it doesn't offer: API keys, placing orders (it's built
//! without the engine's live trading), and deleting or overwriting your data.

#![recursion_limit = "512"]

pub mod cache;
pub mod data;
pub mod guide;
pub mod http;
pub mod jobs;
pub mod mcp;
pub mod portfolio;
pub mod runs;
pub mod state;
pub mod tools;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use persephone_core::{UnixNanos, time::DateTime};
use persephone_data::store::Store;
use persephone_ml::models::Models;
use persephone_plugin_host::{Host, Plugins, Services};
use serde_json::Value;

/// Where everything is. All of it is under one folder unless told otherwise.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The app's data folder.
    pub data: PathBuf,
    pub store: PathBuf,
    pub plugins: PathBuf,
    pub models: PathBuf,
}

impl Paths {
    /// Everything under `data`, laid out as the app lays it out.
    #[must_use]
    pub fn under(data: impl Into<PathBuf>) -> Self {
        let data = data.into();
        Self {
            store: data.join("store"),
            plugins: data.join("plugins"),
            models: data.join("models"),
            data,
        }
    }

    /// The app's own folders, so Lab sees just what it sees.
    ///
    /// # Errors
    ///
    /// Returns a message if this system has no data folder.
    pub fn default_paths() -> Result<Self, String> {
        let data = dirs::data_dir()
            .ok_or("no data folder on this system")?
            .join("persephone");
        let mut paths = Self::under(data);
        if let Some(store) = Store::default_root() {
            paths.store = store;
        }
        if let Some(models) = Models::default_dir() {
            paths.models = models;
        }
        Ok(paths)
    }

    /// Lab's own files.
    #[must_use]
    pub fn lab(&self) -> PathBuf {
        self.data.join("lab")
    }
}

/// Everything the tools work on.
pub struct Lab {
    pub paths: Paths,
    pub store: Store,
    pub host: Host,
    pub models: Arc<Models>,
    plugins: Mutex<Arc<Plugins>>,
    pub cache: cache::BarCache,
    pub(crate) jobs: Mutex<jobs::Jobs>,
    pub(crate) log: Mutex<()>,
    /// Runs read lately, so reading a run a page at a time doesn't read it from disk each time.
    pub(crate) recent_runs: Mutex<VecDeque<(String, Arc<Value>)>>,
}

impl Lab {
    /// # Errors
    ///
    /// Returns a message if its folders can't be made or the plugin runtime can't start.
    pub fn new(paths: Paths) -> Result<Arc<Self>, String> {
        Self::with_source(paths, Arc::new(cache::Binance::default()))
    }

    /// One whose non-local bars come from `source`, for tests.
    ///
    /// # Errors
    ///
    /// Returns a message if its folders can't be made or the plugin runtime can't start.
    pub fn with_source(
        paths: Paths,
        source: Arc<dyn cache::BarSource>,
    ) -> Result<Arc<Self>, String> {
        for dir in ["runs", "jobs", "results", "exports", "answers"] {
            std::fs::create_dir_all(paths.lab().join(dir)).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(&paths.models).map_err(|e| e.to_string())?;
        let models = Arc::new(Models::new(&paths.models));
        // No language models: a plugin that asks gets no answer, and nothing here can spend money.
        let host = Host::with_services(Services::new(Some(Arc::clone(&models)), None))?;
        let plugins = host.load_dir(&paths.plugins);
        Ok(Arc::new(Self {
            store: Store::new(&paths.store),
            host,
            models,
            plugins: Mutex::new(Arc::new(plugins)),
            cache: cache::BarCache::new(paths.lab().join("cache"), source),
            jobs: Mutex::default(),
            log: Mutex::new(()),
            recent_runs: Mutex::default(),
            paths,
        }))
    }

    /// An answer as text for an agent connected over MCP, whose context it all goes into: one too
    /// big for that is saved to a file instead, saying where and how to ask for less.
    #[must_use]
    pub fn fit_for_agent(&self, answer: &Value) -> String {
        let text = answer.to_string();
        if text.len() <= MOST_ANSWER_BYTES {
            return text;
        }
        let path = self
            .paths
            .lab()
            .join("answers")
            .join(format!("{}.json", new_id("answer")));
        if let Err(error) = std::fs::write(&path, &text) {
            return serde_json::json!({
                "too_large": true,
                "bytes": text.len(),
                "hint": format!("It couldn't be saved ({error}). Ask for less with `limit`."),
            })
            .to_string();
        }
        serde_json::json!({
            "too_large": true,
            "bytes": text.len(),
            "path": path,
            "hint": "That's too much to read here, so the whole answer is in that file. Read it from there, or ask for less with `limit`.",
        })
        .to_string()
    }

    /// The plugins loaded now.
    #[must_use]
    pub fn plugins(&self) -> Arc<Plugins> {
        self.plugins
            .lock()
            .map_or_else(|_| Arc::new(Plugins::default()), |p| Arc::clone(&p))
    }

    /// Loads the plugins folder again, after something's been added to it.
    pub(crate) fn reload_plugins(&self) {
        let loaded = self.host.load_dir(&self.paths.plugins);
        if let Ok(mut plugins) = self.plugins.lock() {
            *plugins = Arc::new(loaded);
        }
    }
}

/// The largest answer an agent over MCP gets directly, about 25,000 tokens.
const MOST_ANSWER_BYTES: usize = 100_000;

/// Unix milliseconds now.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// An id no other run or job has, that sorts by when it was made.
#[must_use]
pub fn new_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}-{}",
        now_ms(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A time from an argument: `2024-01-31`, `2024-01-31 09:30` (or with a `T`), both UTC, or a whole
/// number of nanoseconds since 1970.
///
/// # Errors
///
/// Returns a message saying what would be right.
pub fn parse_time(value: &Value) -> Result<UnixNanos, String> {
    persephone_research::portfolio::moment(value)
}

/// A time as tools show it: `2024-01-31 09:30`.
#[must_use]
pub fn time_text(ts: UnixNanos) -> String {
    let at = DateTime::from_nanos(ts);
    format!("{} {}", at.date(), at.time())
}

#[cfg(test)]
mod tests {
    use persephone_core::time::NANOS_PER_MINUTE;
    use serde_json::json;

    use super::*;

    #[test]
    fn times_read_every_way_and_print_back() {
        let day = parse_time(&json!("2024-01-31")).unwrap();
        assert_eq!(time_text(day), "2024-01-31 00:00");
        let minute = parse_time(&json!("2024-01-31 09:30")).unwrap();
        assert_eq!(minute - day, 570 * NANOS_PER_MINUTE);
        assert_eq!(parse_time(&json!("2024-01-31T09:30:00Z")).unwrap(), minute);
        assert_eq!(parse_time(&json!(minute)).unwrap(), minute);
        for bad in [
            json!("yesterday"),
            json!("2024-13-01"),
            json!(1.5),
            json!("2024-01-31 25:00"),
        ] {
            assert!(
                parse_time(&bad).unwrap_err().contains("2024-01-31"),
                "{bad}"
            );
        }
    }
}

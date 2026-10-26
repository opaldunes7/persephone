//! Plugins in the app: where they live, watching their folder for changes, and matching saved
//! plugin indicators and strategies with whatever is loaded.
//!
//! A saved plugin is stored as `plugin:<id>` with its parameter values. Until its plugin
//! loads it keeps those values under a placeholder description; once loaded, the plugin's
//! own description takes over and the values are checked against it.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use iced::futures::{SinkExt, Stream, StreamExt, channel::mpsc};
use persephone_core::{
    indicator::{BuiltIn, IndicatorInfo, IndicatorKind, IndicatorSpec, Placement},
    strategy::{BuiltInStrategy, StrategyInfo, StrategyKind, StrategySpec},
};
use persephone_ml::{
    ai::{self, Ai},
    models::Models,
};
use persephone_plugin_host::{Plugins, Services};

use crate::Message;

const PREFIX: &str = "plugin:";

/// `~/Library/Application Support/persephone/plugins` on macOS, and the platform equivalent
/// elsewhere.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("persephone").join("plugins"))
}

/// The models folder and the language-model client plugins get, set up from
/// `…/persephone/ai.json` and the environment. Whatever can't be set up is left out, and
/// plugins asking for it get an error saying so.
#[must_use]
pub fn services() -> Services {
    let models = Models::default_dir()
        .filter(|dir| fs::create_dir_all(dir).is_ok())
        .map(|dir| Arc::new(Models::new(dir)));
    let ai = ai::default_paths().and_then(|(config, cache)| {
        let config = ai::Config::load(&config)
            .map_err(|e| eprintln!("persephone: AI is off: {e:#}"))
            .ok()?;
        Ai::new(config, cache).ok().map(Arc::new)
    });
    Services::new(models, ai)
}

/// Where the plugins allowed to call language models are listed.
#[must_use]
pub fn grants_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("persephone").join("ai-grants.json"))
}

/// The plugin ids allowed to call language models; none if the file is missing or unreadable.
#[must_use]
pub fn load_grants(path: &Path) -> BTreeSet<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// # Errors
///
/// Returns an error if the file can't be written.
pub fn save_grants(path: &Path, grants: &BTreeSet<String>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(grants)? + "\n")
}

/// Emits [`Message::PluginsChanged`] whenever a `.wasm` file in `dir` changes.
pub fn watch(dir: &Path) -> impl Stream<Item = Message> + use<> {
    let dir = dir.to_path_buf();
    iced::stream::channel(8, async move |mut output: mpsc::Sender<Message>| {
        let (tx, mut rx) = mpsc::unbounded();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && event.paths.iter().any(|p| is_plugin(p))
            {
                let _ = tx.unbounded_send(());
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if notify::Watcher::watch(&mut watcher, &dir, notify::RecursiveMode::NonRecursive).is_err()
        {
            return;
        }
        while rx.next().await.is_some() {
            if output.send(Message::PluginsChanged).await.is_err() {
                break;
            }
        }
    })
}

fn is_plugin(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "wasm")
}

/// How an indicator is named in a saved layout.
#[must_use]
pub fn indicator_key(kind: &IndicatorKind) -> String {
    match kind {
        IndicatorKind::BuiltIn(builtin) => builtin.id().into(),
        IndicatorKind::Plugin(id) => format!("{PREFIX}{id}"),
    }
}

/// How a strategy is named in a saved layout.
#[must_use]
pub fn strategy_key(kind: &StrategyKind) -> String {
    match kind {
        StrategyKind::BuiltIn(builtin) => builtin.id().into(),
        StrategyKind::Plugin(id) => format!("{PREFIX}{id}"),
    }
}

/// A saved indicator; plugins come back as placeholders keeping their values.
#[must_use]
pub fn saved_indicator(key: &str, params: &[f64]) -> Option<IndicatorSpec> {
    match key.strip_prefix(PREFIX) {
        Some(id) => Some(IndicatorSpec {
            kind: IndicatorKind::Plugin(id.into()),
            info: Arc::new(IndicatorInfo {
                name: id.into(),
                short_name: id.into(),
                placement: Placement::Overlay,
                params: Vec::new(),
                outputs: Vec::new(),
                fill: None,
                levels: Vec::new(),
                range: None,
            }),
            params: params.to_vec(),
        }),
        None => BuiltIn::from_id(key).map(|kind| IndicatorSpec::with_params(kind, params)),
    }
}

/// A saved strategy; plugins come back as placeholders keeping their values.
#[must_use]
pub fn saved_strategy(key: &str, params: &[f64]) -> Option<StrategySpec> {
    match key.strip_prefix(PREFIX) {
        Some(id) => Some(StrategySpec {
            kind: StrategyKind::Plugin(id.into()),
            info: Arc::new(StrategyInfo {
                name: id.into(),
                description: String::new(),
                params: Vec::new(),
            }),
            params: params.to_vec(),
        }),
        None => BuiltInStrategy::from_id(key).map(|kind| StrategySpec::with_params(kind, params)),
    }
}

/// Re-reads a plugin indicator's description from what's loaded, keeping valid values.
/// Returns `None` for built-ins and plugins that aren't loaded (or haven't changed).
#[must_use]
pub fn refresh_indicator(spec: &IndicatorSpec, plugins: &Plugins) -> Option<IndicatorSpec> {
    let IndicatorKind::Plugin(id) = &spec.kind else {
        return None;
    };
    let plugin = plugins.indicators.get(id)?;
    (*plugin.info != *spec.info).then(|| {
        IndicatorSpec::from_info(spec.kind.clone(), Arc::clone(&plugin.info), &spec.params)
    })
}

/// Re-reads a plugin strategy's description from what's loaded, keeping valid values.
#[must_use]
pub fn refresh_strategy(spec: &StrategySpec, plugins: &Plugins) -> Option<StrategySpec> {
    let StrategyKind::Plugin(id) = &spec.kind else {
        return None;
    };
    let plugin = plugins.strategies.get(id)?;
    (*plugin.info != *spec.info)
        .then(|| StrategySpec::from_info(spec.kind.clone(), Arc::clone(&plugin.info), &spec.params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let ema = IndicatorSpec::new(BuiltIn::Ema);
        assert_eq!(indicator_key(&ema.kind), "ema");
        assert_eq!(saved_indicator("ema", &ema.params), Some(ema));

        let plugin = saved_indicator("plugin:hma", &[16.0]).unwrap();
        assert_eq!(plugin.kind, IndicatorKind::Plugin("hma".into()));
        assert_eq!(
            plugin.params,
            vec![16.0],
            "values are kept until the plugin loads"
        );
        assert_eq!(indicator_key(&plugin.kind), "plugin:hma");

        let turtle = saved_strategy("plugin:turtle", &[55.0, 10.0]).unwrap();
        assert_eq!(strategy_key(&turtle.kind), "plugin:turtle");
        assert!((turtle.size_fraction() - 0.10).abs() < 1e-12);
        assert_eq!(saved_indicator("nope", &[]), None);
    }

    #[test]
    fn unloaded_plugins_are_left_alone() {
        let placeholder = saved_indicator("plugin:hma", &[16.0]).unwrap();
        assert!(refresh_indicator(&placeholder, &Plugins::default()).is_none());
        assert!(
            refresh_indicator(&IndicatorSpec::new(BuiltIn::Sma), &Plugins::default()).is_none()
        );
    }
}

//! Strategies saved for later: what Lab writes and the app shows. Each is one JSON file in a folder
//! both can see.

use std::{
    fs,
    path::{Path, PathBuf},
};

use persephone_core::strategy::{BuiltInStrategy, StrategyInfo, StrategyKind, StrategySpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::{logic::Logic, positions::Positions, settings::Settings};

/// A strategy named in JSON, one of:
/// - `{"builtin": "ema-cross", "params": {"Fast": 5}}`
/// - `{"plugin": "my_strategy", "params": {...}}`
/// - `{"logic": {...a logic tree...}, "params": {...}}`
/// - `{"positions": [[ts, 1], [ts, -1], [ts, 0]], "params": {...}}`
#[derive(Clone, Debug, PartialEq)]
pub enum StrategyRef {
    BuiltIn(BuiltInStrategy),
    Plugin(String),
    Logic(Box<Logic>),
    Positions(Positions),
}

impl StrategyRef {
    /// Reads one, with its parameter values kept apart for [`with_params`].
    ///
    /// # Errors
    ///
    /// Returns a message saying what's wrong with it.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or_else(|| {
            format!(
                "strategy: expected an object with `builtin`, `plugin`, `logic` or `positions`, not {value}"
            )
        })?;
        if let Some(unknown) = object.keys().find(|key| {
            !["builtin", "plugin", "logic", "positions", "params"].contains(&key.as_str())
        }) {
            return Err(format!(
                "strategy: `{unknown}` isn't one of builtin, plugin, logic, positions or params"
            ));
        }
        if let Some(id) = object.get("builtin").and_then(Value::as_str) {
            return BuiltInStrategy::from_id(id)
                .map(Self::BuiltIn)
                .ok_or_else(|| {
                    let known: Vec<&str> = BuiltInStrategy::ALL.iter().map(|s| s.id()).collect();
                    format!("strategy: there's no built-in `{id}`; there are {known:?}")
                });
        }
        if let Some(id) = object.get("plugin").and_then(Value::as_str) {
            return Ok(Self::Plugin(id.to_owned()));
        }
        if let Some(tree) = object.get("logic") {
            return Logic::parse(tree).map(|logic| Self::Logic(Box::new(logic)));
        }
        if let Some(list) = object.get("positions") {
            return Positions::parse(list).map(Self::Positions);
        }
        Err("strategy: it needs `builtin`, `plugin`, `logic` or `positions`".into())
    }
}

/// `info`'s strategy with the values named in `value`'s `params` set; the rest keep their defaults.
///
/// # Errors
///
/// Returns a message if a name isn't one of its parameters or a value is out of range.
pub fn with_params(
    kind: StrategyKind,
    info: Arc<StrategyInfo>,
    value: &Value,
) -> Result<StrategySpec, String> {
    let mut spec = StrategySpec::from_info(kind, Arc::clone(&info), &[]);
    let Some(params) = value.get("params") else {
        return Ok(spec);
    };
    let params = params
        .as_object()
        .ok_or_else(|| format!("strategy.params: expected an object, not {params}"))?;
    for (name, given) in params {
        let index = info
            .params
            .iter()
            .position(|p| p.name == name.as_str())
            .ok_or_else(|| {
                let known: Vec<&str> = info.params.iter().map(|p| p.name.as_ref()).collect();
                format!("strategy.params: `{name}` isn't a parameter; there are {known:?}")
            })?;
        let number = given
            .as_f64()
            .ok_or_else(|| format!("strategy.params.{name}: expected a number, not {given}"))?;
        spec.set_param(index, number)?;
    }
    Ok(spec)
}

/// A strategy saved for later.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedResult {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// As [`StrategyRef::parse`] reads it.
    pub strategy: Value,
    /// The settings it was tested with, if any were given.
    #[serde(default)]
    pub settings: Option<Settings>,
    /// How it did, for showing beside its name.
    #[serde(default)]
    pub summary: Value,
    /// Unix milliseconds.
    pub saved_at: u64,
}

/// Where saved results live, under the app's data folder.
#[must_use]
pub fn results_dir(data_root: &Path) -> PathBuf {
    data_root.join("lab").join("results")
}

/// Every result in `dir`, newest first. A file that can't be read is left out.
#[must_use]
pub fn load_results(dir: &Path) -> Vec<SavedResult> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut results: Vec<SavedResult> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let text = fs::read_to_string(entry.path()).ok()?;
            serde_json::from_str(&text).ok()
        })
        .collect();
    results.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then(a.id.cmp(&b.id)));
    results
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strategies_are_named_three_ways() {
        let parse = |value| StrategyRef::parse(&value);
        assert_eq!(
            parse(json!({"builtin": "ema-cross"})).unwrap(),
            StrategyRef::BuiltIn(BuiltInStrategy::EmaCross)
        );
        assert_eq!(
            parse(json!({"plugin": "mine"})).unwrap(),
            StrategyRef::Plugin("mine".into())
        );
        let logic = parse(json!({"logic": {"long": {"gt": ["close", 1]}}})).unwrap();
        assert!(matches!(logic, StrategyRef::Logic(_)));

        let error = parse(json!({"builtin": "nope"})).unwrap_err();
        assert!(error.contains("rsi-reversion"), "{error}");
        assert!(parse(json!({"params": {}})).unwrap_err().contains("needs"));
        assert!(parse(json!({"plugn": "x"})).unwrap_err().contains("plugn"));
        let tree = parse(json!({"logic": {"long": {"gtt": [1, 2]}}})).unwrap_err();
        assert!(tree.contains("gtt"), "{tree}");
    }

    #[test]
    fn parameters_are_set_by_name() {
        let kind = BuiltInStrategy::EmaCross;
        let info = Arc::new(kind.info());
        let spec = |params: Value| {
            with_params(
                StrategyKind::BuiltIn(kind),
                Arc::clone(&info),
                &json!({"builtin": "ema-cross", "params": params}),
            )
        };
        let set = spec(json!({"Fast": 7, "Size %": 25})).unwrap();
        assert_eq!(set.params[0], 7.0);
        assert_eq!(set.size_fraction(), 0.25);
        assert_eq!(set.params[1], info.params[1].default);

        let unknown = spec(json!({"Fastest": 7})).unwrap_err();
        assert!(unknown.contains("Fast"), "{unknown}");
        assert!(spec(json!({"Fast": 2.5})).is_err());
        assert!(spec(json!({"Fast": "x"})).is_err());
    }

    #[test]
    fn saved_results_come_back_newest_first_skipping_bad_files() {
        let dir = std::env::temp_dir().join(format!("persephone-results-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let result = |id: &str, saved_at| SavedResult {
            id: id.into(),
            name: id.into(),
            note: String::new(),
            strategy: json!({"builtin": "ema-cross"}),
            settings: Some(Settings::default()),
            summary: json!({"net_return": 0.1}),
            saved_at,
        };
        for saved in [result("older", 1), result("newer", 2)] {
            let text = serde_json::to_string(&saved).unwrap();
            fs::write(dir.join(format!("{}.json", saved.id)), text).unwrap();
        }
        fs::write(dir.join("broken.json"), "{").unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();

        let loaded = load_results(&dir);
        assert_eq!(loaded, vec![result("newer", 2), result("older", 1)]);
        assert!(load_results(&dir.join("missing")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}

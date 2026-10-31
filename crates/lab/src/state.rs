//! Tools for what's installed and saved: indicators, models, strategies, plugins, the app's own state,
//! saved results and the call log.

use std::{
    fs::{self, File},
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
    sync::Arc,
};

use persephone_core::{
    indicator::{BuiltIn, IndicatorInfo, IndicatorKind, IndicatorSpec, Param},
    strategy::{BuiltInStrategy, StrategyInfo},
};
use persephone_engine::indicators::compute;
use persephone_ml::models::Tensor;
use persephone_plugin_host::Plugin;
use persephone_research::{
    logic,
    results::{SavedResult, load_results, results_dir},
};
use serde_json::{Value, json};

use crate::{
    Lab,
    data::MarketArgs,
    jobs::check_id,
    new_id, now_ms,
    runs::read_settings,
    time_text,
    tools::{DEFAULT_ROWS, limit_of, page, text},
};

fn params_json(params: &[Param]) -> Vec<Value> {
    params
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "default": p.default,
                "min": p.min,
                "max": p.max,
                "whole_number": p.integer,
            })
        })
        .collect()
}

fn indicator_json(id: &str, info: &IndicatorInfo) -> Value {
    json!({
        "id": id,
        "name": info.name,
        "params": params_json(&info.params),
        "outputs": info.outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>(),
    })
}

fn strategy_json(source: Value, info: &StrategyInfo) -> Value {
    json!({
        "strategy": source,
        "name": info.name,
        "description": info.description,
        "params": params_json(&info.params),
    })
}

/// A saved file in the app's own format, as JSON.
fn read_ron(path: &Path) -> Result<Option<Value>, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut reader = Ron {
        text: text.as_bytes(),
        at: 0,
    };
    let value = reader
        .value()
        .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    Ok(Some(value))
}

/// Reads the app's saved files into JSON. Reading RON's own generic value would lose which choice
/// each enum is, and those are the very data asked for: a bare name becomes a string, `Name(x)`
/// becomes `{"Name": x}`, `Some(x)` is just `x`, and `(a: 1)` is an object.
struct Ron<'a> {
    text: &'a [u8],
    at: usize,
}

impl Ron<'_> {
    fn skip_space(&mut self) {
        while let Some(byte) = self.text.get(self.at) {
            if byte.is_ascii_whitespace() {
                self.at += 1;
            } else if self.text[self.at..].starts_with(b"//") {
                while self.text.get(self.at).is_some_and(|b| *b != b'\n') {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.at))
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => {
                self.at += 1;
                self.list(b']').map(Value::Array)
            }
            Some(b'{') => {
                self.at += 1;
                self.map()
            }
            Some(b'(') => {
                self.at += 1;
                self.parens()
            }
            Some(b'-' | b'+' | b'0'..=b'9') => self.number(),
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'_' => self.named(),
            _ => self.fail("expected a value"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let start = self.at;
        self.at += 1;
        while let Some(byte) = self.text.get(self.at) {
            match byte {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    // RON's escapes are JSON's, near enough for what the app writes.
                    let raw = std::str::from_utf8(&self.text[start..self.at])
                        .map_err(|e| e.to_string())?;
                    return serde_json::from_str(raw).map_err(|e| e.to_string());
                }
                _ => self.at += 1,
            }
        }
        self.fail("a string doesn't end")
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.' | b'_'))
        {
            self.at += 1;
        }
        let raw = std::str::from_utf8(&self.text[start..self.at]).map_err(|e| e.to_string())?;
        serde_json::from_str(raw.trim_start_matches('+'))
            .map_err(|_| format!("`{raw}` isn't a number at byte {start}"))
    }

    fn word(&mut self) -> Result<String, String> {
        self.skip_space();
        // `r#per-order` is how RON writes a name with a dash in it.
        if self.text[self.at..].starts_with(b"r#") {
            self.at += 2;
        }
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            self.at += 1;
        }
        if start == self.at {
            return self.fail("expected a name");
        }
        Ok(String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    /// A bare name, or a name with something after it.
    fn named(&mut self) -> Result<Value, String> {
        let name = self.word()?;
        if self.peek() == Some(b'(') {
            self.at += 1;
            let inner = self.parens()?;
            return Ok(if name == "Some" {
                inner
            } else {
                json!({ name: inner })
            });
        }
        Ok(match name.as_str() {
            "true" => json!(true),
            "false" => json!(false),
            "None" => Value::Null,
            _ => json!(name),
        })
    }

    fn list(&mut self, end: u8) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        loop {
            if self.eat(end) {
                return Ok(items);
            }
            items.push(self.value()?);
            if !self.eat(b',') && self.peek() != Some(end) {
                return self.fail("expected a comma");
            }
        }
    }

    fn map(&mut self) -> Result<Value, String> {
        let mut object = serde_json::Map::new();
        loop {
            if self.eat(b'}') {
                return Ok(Value::Object(object));
            }
            let key = match self.value()? {
                Value::String(key) => key,
                other => other.to_string(),
            };
            if !self.eat(b':') {
                return self.fail("expected a colon");
            }
            object.insert(key, self.value()?);
            if !self.eat(b',') && self.peek() != Some(b'}') {
                return self.fail("expected a comma");
            }
        }
    }

    /// After a `(`: fields, one value, or several.
    fn parens(&mut self) -> Result<Value, String> {
        if self.eat(b')') {
            return Ok(Value::Null);
        }
        let mut object = serde_json::Map::new();
        let mut items = Vec::new();
        loop {
            if self.eat(b')') {
                break;
            }
            let start = self.at;
            // A field is a name and a colon; anything else is a value.
            let field = self.word().ok().filter(|_| self.eat(b':'));
            match field {
                Some(name) => {
                    object.insert(name, self.value()?);
                }
                None => {
                    self.at = start;
                    items.push(self.value()?);
                }
            }
            if !self.eat(b',') && self.peek() != Some(b')') {
                return self.fail("expected a comma");
            }
        }
        Ok(if !object.is_empty() {
            Value::Object(object)
        } else if items.len() == 1 {
            items.remove(0)
        } else {
            Value::Array(items)
        })
    }
}

impl Lab {
    fn layout_path(&self) -> std::path::PathBuf {
        self.paths.data.join("layout.ron")
    }

    pub(crate) fn list_indicators(&self) -> Result<Value, String> {
        let plugins = self.plugins();
        let mut indicators: Vec<Value> = BuiltIn::ALL
            .iter()
            .map(|kind| indicator_json(kind.id(), &kind.info()))
            .collect();
        indicators.extend(
            plugins
                .indicators
                .iter()
                .map(|(id, plugin)| indicator_json(id, &plugin.info)),
        );
        Ok(json!({"indicators": indicators, "plugin_errors": plugins.errors}))
    }

    pub(crate) fn compute_indicator(&self, args: &Value) -> Result<Value, String> {
        let market_args = MarketArgs::parse(args)?;
        let id = text(args, "indicator")?;
        let plugins = self.plugins();
        let (kind, info) = match BuiltIn::from_id(id) {
            Some(kind) => (IndicatorKind::BuiltIn(kind), Arc::new(kind.info())),
            None => {
                let plugin = plugins.indicators.get(id).ok_or_else(|| {
                    format!("`indicator`: there's no `{id}`; list_indicators lists them")
                })?;
                (
                    IndicatorKind::Plugin(id.to_owned()),
                    Arc::clone(&plugin.info),
                )
            }
        };
        let mut spec = IndicatorSpec::from_info(kind, Arc::clone(&info), &[]);
        if let Some(given) = args.get("params").and_then(Value::as_object) {
            for (name, value) in given {
                let index = info
                    .params
                    .iter()
                    .position(|p| p.name == name.as_str())
                    .ok_or_else(|| format!("`params`: `{name}` isn't one of this indicator's"))?;
                let value = value
                    .as_f64()
                    .ok_or_else(|| format!("`params.{name}`: expected a number, not {value}"))?;
                spec.set_param(index, value)?;
            }
        }
        let market = self.load_market(&market_args)?;
        let series = match plugins
            .indicators
            .get(id)
            .filter(|_| BuiltIn::from_id(id).is_none())
        {
            Some(plugin) => plugin.compute(&spec.params, &market.candles)?,
            None => compute(&spec, &market.candles)?,
        };
        let from = market_args.from.unwrap_or(0);
        let rows: Vec<Value> = market
            .candles
            .iter()
            .enumerate()
            .filter(|(_, candle)| candle.ts >= from)
            .map(|(i, candle)| {
                let mut row = vec![json!(candle.ts), json!(time_text(candle.ts))];
                row.extend(series.iter().map(|values| json!(values[i])));
                Value::Array(row)
            })
            .collect();
        let mut columns: Vec<String> = vec!["ts".into(), "time".into()];
        columns.extend(info.outputs.iter().map(|o| o.name.to_string()));
        let names: Vec<&str> = columns.iter().map(String::as_str).collect();
        Ok(page(&names, rows, args, DEFAULT_ROWS))
    }

    pub(crate) fn list_models(&self) -> Result<Value, String> {
        Ok(json!({"models": self.models.names(), "folder": self.models.dir()}))
    }

    pub(crate) fn run_model(&self, args: &Value) -> Result<Value, String> {
        let numbers = |name: &str| {
            args.get(name)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let shape: Vec<usize> = numbers("shape")?
            .iter()
            .map(|n| {
                n.as_u64()
                    .map(|n| n as usize)
                    .ok_or_else(|| format!("`shape`: {n} isn't a size"))
            })
            .collect::<Result<_, _>>()?;
        let data: Vec<f32> = numbers("data")?
            .iter()
            .map(|n| {
                n.as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| format!("`data`: {n} isn't a number"))
            })
            .collect::<Result<_, _>>()?;
        let outputs = self
            .models
            .run(text(args, "model")?, &Tensor { shape, data })?;
        let outputs: Vec<Value> = outputs
            .iter()
            .map(|tensor| json!({"shape": tensor.shape, "data": tensor.data}))
            .collect();
        Ok(json!({"outputs": outputs}))
    }

    pub(crate) fn list_strategies(&self) -> Result<Value, String> {
        let plugins = self.plugins();
        let mut strategies: Vec<Value> = BuiltInStrategy::ALL
            .iter()
            .map(|kind| strategy_json(json!({"builtin": kind.id()}), &kind.info()))
            .collect();
        strategies.extend(
            plugins
                .strategies
                .iter()
                .map(|(id, plugin)| strategy_json(json!({"plugin": id}), &plugin.info)),
        );
        Ok(json!({"strategies": strategies, "plugin_errors": plugins.errors}))
    }

    pub(crate) fn describe_strategy(&self, args: &Value) -> Result<Value, String> {
        let (spec, _) = self.resolve(args.get("strategy").unwrap_or(&Value::Null))?;
        Ok(json!({
            "name": spec.info.name,
            "description": spec.info.description,
            "params": params_json(&spec.info.params),
            "values": spec.params,
        }))
    }

    pub(crate) fn validate_logic(args: &Value) -> Value {
        let allowed = json!({
            "fields": logic::FIELDS,
            "indicators": logic::INDICATORS,
            "operations": logic::OPERATIONS,
            "comparisons": logic::COMPARISONS,
            "combinations": logic::COMBINATIONS,
            "rules": "`long` and `short` ask for that position; on a bar with both or neither, `flat` asks to close, and otherwise the position stays.",
            "values": "A number, a field, a parameter as `$name`, or an object: {\"ema\": {\"of\": \"close\", \"length\": \"$fast\"}}, {\"ago\": {\"of\": \"close\", \"bars\": 1}}, {\"add\": [a, b]}.",
            "example": {
                "name": "Fast over slow",
                "params": {"fast": 10, "slow": 30},
                "long": {"crosses_above": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
                "short": {"crosses_below": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
            },
        });
        let error = args
            .get("logic")
            .and_then(|tree| logic::Logic::parse(tree).err());
        json!({"valid": args.get("logic").is_some() && error.is_none(), "error": error, "allowed": allowed})
    }

    /// Adds a program answering over a socket: it has to answer before it goes anywhere the app
    /// would pick it up.
    fn install_external(&self, name: &str, address: &str) -> Result<Value, String> {
        check_id(name)
            .map_err(|_| format!("`name`: `{name}` should be letters, digits, - or _"))?;
        let dest = self.paths.plugins.join(format!("{name}.external"));
        if dest.exists() {
            return Err(format!(
                "{} already exists; Lab won't overwrite it",
                dest.display()
            ));
        }
        let trying = self.paths.lab().join(new_id("external"));
        fs::create_dir_all(&trying).map_err(|e| e.to_string())?;
        let source = trying.join(format!("{name}.external"));
        let tried = fs::write(&source, format!("{address}\n"))
            .map_err(|e| e.to_string())
            .and_then(|()| self.host.load(&source));
        let installed = tried.and_then(|plugin| {
            let id = match plugin {
                Plugin::Indicator(plugin) => plugin.id.clone(),
                Plugin::Strategy(plugin) => plugin.id.clone(),
            };
            fs::create_dir_all(&self.paths.plugins).map_err(|e| e.to_string())?;
            fs::copy(&source, &dest).map_err(|e| e.to_string())?;
            Ok(id)
        });
        let _ = fs::remove_dir_all(&trying);
        let id = installed?;
        self.reload_plugins();
        Ok(json!({"id": id, "kind": "strategy", "path": dest}))
    }

    pub(crate) fn install_plugin(&self, args: &Value) -> Result<Value, String> {
        if let (Some(name), Some(address)) = (
            args.get("name").and_then(Value::as_str),
            args.get("address").and_then(Value::as_str),
        ) {
            return self.install_external(name, address);
        }
        let source = Path::new(
            args.get("path")
                .and_then(Value::as_str)
                .ok_or("it needs `path`, or `name` and `address`")?,
        );
        let name = source
            .file_name()
            .filter(|_| source.extension().is_some_and(|ext| ext == "wasm"))
            .ok_or_else(|| format!("`path`: {} isn't a .wasm file", source.display()))?;
        // It has to load before it goes anywhere the app would pick it up.
        let (id, kind) = match self.host.load(source)? {
            Plugin::Indicator(plugin) => (plugin.id.clone(), "indicator"),
            Plugin::Strategy(plugin) => (plugin.id.clone(), "strategy"),
        };
        fs::create_dir_all(&self.paths.plugins).map_err(|e| e.to_string())?;
        let dest = self.paths.plugins.join(name);
        if dest.exists() {
            return Err(format!(
                "{} already exists; Lab won't overwrite it",
                dest.display()
            ));
        }
        fs::copy(source, &dest).map_err(|e| e.to_string())?;
        self.reload_plugins();
        Ok(json!({"id": id, "kind": kind, "path": dest}))
    }

    pub(crate) fn get_layout(&self) -> Result<Value, String> {
        Ok(json!({"layout": read_ron(&self.layout_path())?}))
    }

    pub(crate) fn get_chart_settings(&self) -> Result<Value, String> {
        let layout = read_ron(&self.layout_path())?.unwrap_or(Value::Null);
        let mut charts: Vec<Value> = layout["tiles"].as_array().cloned().unwrap_or_default();
        if let Some(others) = layout["others"].as_array() {
            for workspace in others {
                charts.extend(workspace["tiles"].as_array().cloned().unwrap_or_default());
            }
        }
        Ok(json!({"charts": charts}))
    }

    pub(crate) fn get_drawings(&self) -> Result<Value, String> {
        Ok(json!({"drawings": read_ron(&self.paths.data.join("drawings.ron"))?}))
    }

    pub(crate) fn save_result(&self, args: &Value) -> Result<Value, String> {
        let strategy = args.get("strategy").cloned().unwrap_or(Value::Null);
        self.resolve(&strategy)?;
        if strategy.get("positions").is_some() {
            return Err(
                "`strategy`: a list of positions only fits the bars it was made for, so it can't be saved for the app's charts".into(),
            );
        }
        let settings = args.get("settings").map(read_settings).transpose()?;
        let summary = match args.get("run_id").and_then(Value::as_str) {
            Some(id) => self.load_run(id)?["summary"].clone(),
            None => Value::Null,
        };
        let id = new_id("result");
        let result = SavedResult {
            id: id.clone(),
            name: text(args, "name")?.to_owned(),
            note: args
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            strategy,
            settings,
            summary,
            saved_at: now_ms(),
        };
        let dir = results_dir(&self.paths.data);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{id}.json"));
        let text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
        // Written aside and moved, so the app never reads half a file.
        let partial = dir.join(format!("{id}.partial"));
        fs::write(&partial, text)
            .and_then(|()| fs::rename(&partial, &path))
            .map_err(|e| e.to_string())?;
        Ok(json!({"id": id, "path": path}))
    }

    pub(crate) fn list_results(&self) -> Value {
        json!({"results": load_results(&results_dir(&self.paths.data))})
    }

    pub(crate) fn get_call_log(&self, args: &Value) -> Result<Value, String> {
        let only = args.get("tool").and_then(Value::as_str);
        let before = args
            .get("cursor")
            .and_then(Value::as_str)
            .and_then(|cursor| cursor.parse().ok());
        let (rows, next) = read_log_back(
            &self.paths.lab().join("calls.jsonl"),
            before,
            limit_of(args, LOGGED_CALLS),
            |row| only.is_none_or(|tool| row["tool"] == tool),
        );
        Ok(json!({
            "columns": ["at", "tool", "args", "ok", "error"],
            "rows": rows,
            "next_cursor": next.map(|at: u64| at.to_string()),
        }))
    }
}

/// How many calls `get_call_log` gives unless asked for more.
const LOGGED_CALLS: usize = 50;
/// How much of the log to read at a time, going back from its end.
const LOG_CHUNK: u64 = 256 * 1024;

/// The newest lines before byte `before` (the end if there's none) that `keep` accepts, up to `most`,
/// with where to carry on. It reads only as far back as it needs to, as the log only ever grows.
fn read_log_back(
    path: &Path,
    before: Option<u64>,
    most: usize,
    keep: impl Fn(&Value) -> bool,
) -> (Vec<Value>, Option<u64>) {
    let mut rows = Vec::new();
    let Ok(mut file) = File::open(path) else {
        return (rows, None);
    };
    let size = file.metadata().map_or(0, |meta| meta.len());
    let mut at = before.unwrap_or(size).min(size);
    // The start of a line whose beginning is in a chunk not read yet.
    let mut rest = Vec::new();
    while at > 0 {
        let length = LOG_CHUNK.min(at);
        at -= length;
        let mut chunk = vec![0; length as usize];
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut chunk).is_err() {
            break;
        }
        chunk.append(&mut rest);
        let mut end = chunk.len();
        loop {
            let start = chunk[..end]
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map(|newline| newline + 1);
            // The first line in a chunk may go back further, unless this is the file's start.
            let Some(line_start) = start.or((at == 0).then_some(0)) else {
                break;
            };
            let line = &chunk[line_start..end];
            if let Ok(row) = serde_json::from_slice::<Value>(line)
                && keep(&row)
            {
                rows.push(row);
                if rows.len() == most {
                    let next = at + line_start as u64;
                    return (rows, (next > 0).then_some(next));
                }
            }
            if line_start == 0 {
                end = 0;
                break;
            }
            end = line_start - 1;
        }
        chunk.truncate(end);
        rest = chunk;
    }
    (rows, None)
}

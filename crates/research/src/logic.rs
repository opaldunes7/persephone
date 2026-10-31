//! Strategies as JSON, so they can be tried without compiling anything:
//!
//! ```json
//! {
//!   "name": "Fast over slow",
//!   "params": { "fast": 10, "slow": 30 },
//!   "long":  { "crosses_above": [{ "ema": { "length": "$fast" } }, { "ema": { "length": "$slow" } }] },
//!   "short": { "crosses_below": [{ "ema": { "length": "$fast" } }, { "ema": { "length": "$slow" } }] }
//! }
//! ```
//!
//! A value is a number, a bar's field (`open`, `high`, `low`, `close`, `volume`), a parameter
//! (`$name`), or an object: an indicator (`sma`, `ema`, `rsi`, `atr`, `highest`, `lowest`, `roc`,
//! `zscore`), an earlier value (`ago`), or arithmetic (`add`, `sub`, `mul`, `div`). A rule compares
//! values (`gt`, `lt`, `ge`, `le`, `crosses_above`, `crosses_below`) or combines rules (`and`, `or`,
//! `not`).
//!
//! `long` and `short` ask for that position; on a bar with both or neither, `flat` asks to close,
//! and otherwise the position stays as it is. Everything runs one closed bar at a time, so a
//! strategy can't look ahead. A value not ready yet makes any comparison on it false.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    sync::Arc,
};

use persephone_core::{
    Candle,
    indicator::Param,
    signal::{Signal, SignalFactory},
    strategy::{StrategyInfo, StrategyKind, StrategySpec},
};
use persephone_engine::indicators::Streaming;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The most nodes a tree may have, and the longest look-back it may ask for.
pub const MAX_NODES: usize = 500;
pub const MAX_LENGTH: f64 = 10_000.0;

/// What [`Logic::parse`] accepts, for telling an agent what it can write.
pub const FIELDS: [&str; 5] = ["open", "high", "low", "close", "volume"];
pub const INDICATORS: [&str; 8] = [
    "sma", "ema", "rsi", "atr", "highest", "lowest", "roc", "zscore",
];
pub const OPERATIONS: [&str; 5] = ["ago", "add", "sub", "mul", "div"];
pub const COMPARISONS: [&str; 6] = ["gt", "lt", "ge", "le", "crosses_above", "crosses_below"];
pub const COMBINATIONS: [&str; 3] = ["and", "or", "not"];

/// A number written out, or a parameter's name.
#[derive(Clone, Debug, PartialEq)]
enum Amount {
    Fixed(f64),
    Param(String),
}

/// A value on each bar.
#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Number(Amount),
    Field(Field),
    Indicator {
        kind: IndicatorKind,
        of: Box<Self>,
        length: Amount,
    },
    Ago {
        of: Box<Self>,
        bars: Amount,
    },
    Arithmetic(Arithmetic, Box<Self>, Box<Self>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Open,
    High,
    Low,
    Close,
    Volume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndicatorKind {
    Sma,
    Ema,
    Rsi,
    Atr,
    Highest,
    Lowest,
    Roc,
    Zscore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arithmetic {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Gt,
    Lt,
    Ge,
    Le,
    CrossesAbove,
    CrossesBelow,
}

/// A yes or no on each bar.
#[derive(Clone, Debug, PartialEq)]
enum Rule {
    Compare(Comparison, Expr, Expr),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

/// A strategy written as a logic tree, checked and ready to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Logic {
    name: String,
    source: Value,
    params: BTreeMap<String, f64>,
    /// Parameters used as look-backs, so whole numbers only.
    lengths: BTreeSet<String>,
    long: Option<Rule>,
    short: Option<Rule>,
    flat: Option<Rule>,
}

struct Parser<'a> {
    params: &'a BTreeMap<String, f64>,
    lengths: BTreeSet<String>,
    nodes: usize,
}

/// `at`, then one step further in.
fn deeper(at: &str, step: &str) -> String {
    if step.starts_with('[') {
        format!("{at}{step}")
    } else {
        format!("{at}.{step}")
    }
}

impl Parser<'_> {
    fn count(&mut self, at: &str) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(format!("{at}: the tree has more than {MAX_NODES} nodes"));
        }
        Ok(())
    }

    fn param(&self, name: &str, at: &str) -> Result<Amount, String> {
        if !self.params.contains_key(name) {
            let known: Vec<&str> = self.params.keys().map(String::as_str).collect();
            return Err(format!(
                "{at}: `${name}` isn't in `params`, which has {known:?}"
            ));
        }
        Ok(Amount::Param(name.to_owned()))
    }

    fn expr(&mut self, value: &Value, at: &str) -> Result<Expr, String> {
        self.count(at)?;
        match value {
            Value::Number(number) => number
                .as_f64()
                .map(|n| Expr::Number(Amount::Fixed(n)))
                .ok_or_else(|| format!("{at}: `{number}` isn't a usable number")),
            Value::String(text) => {
                if let Some(name) = text.strip_prefix('$') {
                    return self.param(name, at).map(Expr::Number);
                }
                let field = match text.as_str() {
                    "open" => Field::Open,
                    "high" => Field::High,
                    "low" => Field::Low,
                    "close" => Field::Close,
                    "volume" => Field::Volume,
                    other => {
                        return Err(format!(
                            "{at}: `{other}` isn't a value. Use one of {FIELDS:?}, a parameter as `$name`, a number or an object"
                        ));
                    }
                };
                Ok(Expr::Field(field))
            }
            Value::Object(object) => self.expr_object(object, at),
            other => Err(format!(
                "{at}: expected a value (a number, a field, `$param` or an object), not {other}"
            )),
        }
    }

    fn expr_object(&mut self, object: &Map<String, Value>, at: &str) -> Result<Expr, String> {
        let (key, inner) = single(object, at)?;
        let here = deeper(at, key);
        let kind = match key {
            "sma" => Some(IndicatorKind::Sma),
            "ema" => Some(IndicatorKind::Ema),
            "rsi" => Some(IndicatorKind::Rsi),
            "atr" => Some(IndicatorKind::Atr),
            "highest" => Some(IndicatorKind::Highest),
            "lowest" => Some(IndicatorKind::Lowest),
            "roc" => Some(IndicatorKind::Roc),
            "zscore" => Some(IndicatorKind::Zscore),
            _ => None,
        };
        if let Some(kind) = kind {
            return self.indicator(kind, inner, &here);
        }
        let arithmetic = match key {
            "add" => Some(Arithmetic::Add),
            "sub" => Some(Arithmetic::Sub),
            "mul" => Some(Arithmetic::Mul),
            "div" => Some(Arithmetic::Div),
            _ => None,
        };
        if let Some(arithmetic) = arithmetic {
            let (a, b) = self.pair(inner, &here)?;
            return Ok(Expr::Arithmetic(arithmetic, Box::new(a), Box::new(b)));
        }
        if key == "ago" {
            let fields = fields_of(inner, &here, &["of", "bars"])?;
            let of = self.expr(required(fields, "of", &here)?, &deeper(&here, "of"))?;
            let bars = self.amount(
                required(fields, "bars", &here)?,
                &deeper(&here, "bars"),
                0.0,
            )?;
            return Ok(Expr::Ago {
                of: Box::new(of),
                bars,
            });
        }
        Err(format!(
            "{at}: `{key}` isn't a value. Use an indicator {INDICATORS:?} or an operation {OPERATIONS:?}"
        ))
    }

    fn indicator(&mut self, kind: IndicatorKind, inner: &Value, at: &str) -> Result<Expr, String> {
        let allowed: &[&str] = if kind == IndicatorKind::Atr {
            &["length"]
        } else {
            &["of", "length"]
        };
        let fields = fields_of(inner, at, allowed)?;
        let length = self.amount(required(fields, "length", at)?, &deeper(at, "length"), 1.0)?;
        // Most indicators read the close unless told otherwise.
        let of = match fields.get("of") {
            Some(of) => self.expr(of, &deeper(at, "of"))?,
            None => Expr::Field(Field::Close),
        };
        Ok(Expr::Indicator {
            kind,
            of: Box::new(of),
            length,
        })
    }

    /// A look-back: a whole number from `least`, or a parameter.
    fn amount(&mut self, value: &Value, at: &str, least: f64) -> Result<Amount, String> {
        if let Some(name) = value.as_str().and_then(|text| text.strip_prefix('$')) {
            let amount = self.param(name, at)?;
            self.lengths.insert(name.to_owned());
            return Ok(amount);
        }
        let number = value
            .as_f64()
            .ok_or_else(|| format!("{at}: expected a whole number or `$param`, not {value}"))?;
        check_length(number, least).map_err(|error| format!("{at}: {error}"))?;
        Ok(Amount::Fixed(number))
    }

    fn pair(&mut self, value: &Value, at: &str) -> Result<(Expr, Expr), String> {
        let items = value
            .as_array()
            .filter(|items| items.len() == 2)
            .ok_or_else(|| format!("{at}: expected a list of two values, not {value}"))?;
        Ok((
            self.expr(&items[0], &deeper(at, "[0]"))?,
            self.expr(&items[1], &deeper(at, "[1]"))?,
        ))
    }

    fn rule(&mut self, value: &Value, at: &str) -> Result<Rule, String> {
        self.count(at)?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{at}: expected a rule (an object), not {value}"))?;
        let (key, inner) = single(object, at)?;
        let here = deeper(at, key);
        let comparison = match key {
            "gt" => Some(Comparison::Gt),
            "lt" => Some(Comparison::Lt),
            "ge" => Some(Comparison::Ge),
            "le" => Some(Comparison::Le),
            "crosses_above" => Some(Comparison::CrossesAbove),
            "crosses_below" => Some(Comparison::CrossesBelow),
            _ => None,
        };
        if let Some(comparison) = comparison {
            let (a, b) = self.pair(inner, &here)?;
            return Ok(Rule::Compare(comparison, a, b));
        }
        match key {
            "and" | "or" => {
                let items = inner
                    .as_array()
                    .filter(|items| !items.is_empty())
                    .ok_or_else(|| format!("{here}: expected a list of rules, not {inner}"))?;
                let rules = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| self.rule(item, &deeper(&here, &format!("[{i}]"))))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(if key == "and" {
                    Rule::And(rules)
                } else {
                    Rule::Or(rules)
                })
            }
            "not" => Ok(Rule::Not(Box::new(self.rule(inner, &here)?))),
            other => Err(format!(
                "{at}: `{other}` isn't a rule. Use a comparison {COMPARISONS:?} or a combination {COMBINATIONS:?}"
            )),
        }
    }
}

/// The one key of an object that should have just one.
fn single<'a>(object: &'a Map<String, Value>, at: &str) -> Result<(&'a str, &'a Value), String> {
    let mut entries = object.iter();
    match (entries.next(), entries.next()) {
        (Some((key, value)), None) => Ok((key.as_str(), value)),
        _ => {
            let keys: Vec<&String> = object.keys().collect();
            Err(format!("{at}: expected one key, found {keys:?}"))
        }
    }
}

fn fields_of<'a>(
    value: &'a Value,
    at: &str,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let fields = value
        .as_object()
        .ok_or_else(|| format!("{at}: expected an object with {allowed:?}, not {value}"))?;
    if let Some(unknown) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{at}: `{unknown}` isn't one of {allowed:?}"));
    }
    Ok(fields)
}

fn required<'a>(fields: &'a Map<String, Value>, key: &str, at: &str) -> Result<&'a Value, String> {
    fields
        .get(key)
        .ok_or_else(|| format!("{at}: `{key}` is missing"))
}

fn check_length(value: f64, least: f64) -> Result<(), String> {
    if value.fract() != 0.0 || value < least || value > MAX_LENGTH {
        return Err(format!(
            "expected a whole number from {least} to {MAX_LENGTH}, not {value}"
        ));
    }
    Ok(())
}

impl Logic {
    /// Reads and checks a tree.
    ///
    /// # Errors
    ///
    /// Returns a message saying where it's wrong and what would be right.
    pub fn parse(source: &Value) -> Result<Self, String> {
        let fields = fields_of(
            source,
            "strategy",
            &["name", "params", "long", "short", "flat"],
        )?;
        let mut params = BTreeMap::new();
        if let Some(given) = fields.get("params") {
            let given = given
                .as_object()
                .ok_or_else(|| format!("params: expected an object, not {given}"))?;
            for (name, value) in given {
                let value = value
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("params.{name}: expected a number, not {value}"))?;
                params.insert(name.clone(), value);
            }
        }
        let mut parser = Parser {
            params: &params,
            lengths: BTreeSet::new(),
            nodes: 0,
        };
        let mut rule = |key: &str| {
            fields
                .get(key)
                .map(|value| parser.rule(value, key))
                .transpose()
        };
        let (long, short, flat) = (rule("long")?, rule("short")?, rule("flat")?);
        if long.is_none() && short.is_none() {
            return Err(
                "strategy: it needs a `long` or `short` rule, or it can never trade".into(),
            );
        }
        let lengths = parser.lengths;
        for name in &lengths {
            check_length(params[name], 0.0)
                .map_err(|error| format!("params.{name}: it's used as a look-back: {error}"))?;
        }
        let name = fields
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Logic tree")
            .to_owned();
        Ok(Self {
            name,
            source: source.clone(),
            params,
            lengths,
            long,
            short,
            flat,
        })
    }

    /// The tree as it was written.
    #[must_use]
    pub const fn source(&self) -> &Value {
        &self.source
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A short id for this exact tree: the same tree always has the same one.
    #[must_use]
    pub fn id(&self) -> String {
        let digest = Sha256::digest(self.source.to_string());
        let mut id = String::from("logic-");
        for byte in &digest[..8] {
            let _ = write!(id, "{byte:02x}");
        }
        id
    }

    /// Its description, with its parameters to set like any strategy's.
    #[must_use]
    pub fn info(&self) -> StrategyInfo {
        let params = self
            .params
            .iter()
            .map(|(name, &default)| {
                let length = self.lengths.contains(name);
                Param {
                    name: name.clone().into(),
                    default,
                    min: if length { 0.0 } else { -1e12 },
                    max: if length { MAX_LENGTH } else { 1e12 },
                    integer: length,
                }
            })
            .collect();
        StrategyInfo::new(
            self.name.clone(),
            "A strategy written as a logic tree",
            params,
        )
    }

    /// The strategy to put in a backtest request, with this as its plugin.
    #[must_use]
    pub fn spec(&self) -> StrategySpec {
        StrategySpec::from_info(StrategyKind::Plugin(self.id()), Arc::new(self.info()), &[])
    }
}

impl SignalFactory for Logic {
    fn create(&self, params: &[f64]) -> Result<Box<dyn Signal>, String> {
        let values: BTreeMap<&str, f64> = self
            .params
            .iter()
            .enumerate()
            .map(|(i, (name, &default))| (name.as_str(), params.get(i).copied().unwrap_or(default)))
            .collect();
        let build = |rule: &Option<Rule>| {
            rule.as_ref()
                .map(|rule| LiveRule::new(rule, &values))
                .transpose()
        };
        Ok(Box::new(Running {
            long: build(&self.long)?,
            short: build(&self.short)?,
            flat: build(&self.flat)?,
        }))
    }
}

fn amount(amount: &Amount, values: &BTreeMap<&str, f64>) -> f64 {
    match amount {
        Amount::Fixed(value) => *value,
        Amount::Param(name) => values.get(name.as_str()).copied().unwrap_or(f64::NAN),
    }
}

fn length_of(given: &Amount, values: &BTreeMap<&str, f64>, least: f64) -> Result<usize, String> {
    let value = amount(given, values);
    check_length(value, least)?;
    Ok(value as usize)
}

/// A value being worked out bar by bar.
enum LiveExpr {
    Number(f64),
    Field(Field),
    Engine(Streaming, Box<Self>),
    Window {
        kind: IndicatorKind,
        of: Box<Self>,
        window: VecDeque<f64>,
        size: usize,
    },
    Ago {
        of: Box<Self>,
        window: VecDeque<f64>,
        bars: usize,
    },
    Arithmetic(Arithmetic, Box<Self>, Box<Self>),
}

impl LiveExpr {
    fn new(expr: &Expr, values: &BTreeMap<&str, f64>) -> Result<Self, String> {
        Ok(match expr {
            Expr::Number(given) => Self::Number(amount(given, values)),
            Expr::Field(field) => Self::Field(*field),
            Expr::Indicator { kind, of, length } => {
                let period = length_of(length, values, 1.0)?;
                let of = Box::new(Self::new(of, values)?);
                match kind {
                    IndicatorKind::Sma => Self::Engine(Streaming::sma(period), of),
                    IndicatorKind::Ema => Self::Engine(Streaming::ema(period), of),
                    IndicatorKind::Rsi => Self::Engine(Streaming::rsi(period), of),
                    IndicatorKind::Atr => Self::Engine(Streaming::atr(period), of),
                    // A rate of change compares with the value that many bars back.
                    IndicatorKind::Roc => Self::Window {
                        kind: *kind,
                        of,
                        window: VecDeque::new(),
                        size: period + 1,
                    },
                    IndicatorKind::Highest | IndicatorKind::Lowest | IndicatorKind::Zscore => {
                        Self::Window {
                            kind: *kind,
                            of,
                            window: VecDeque::new(),
                            size: period,
                        }
                    }
                }
            }
            Expr::Ago { of, bars } => Self::Ago {
                of: Box::new(Self::new(of, values)?),
                window: VecDeque::new(),
                bars: length_of(bars, values, 0.0)?,
            },
            Expr::Arithmetic(arithmetic, a, b) => Self::Arithmetic(
                *arithmetic,
                Box::new(Self::new(a, values)?),
                Box::new(Self::new(b, values)?),
            ),
        })
    }

    /// The value at this bar, `NaN` if it isn't ready yet.
    fn next(&mut self, bar: &Candle) -> f64 {
        match self {
            Self::Number(value) => *value,
            Self::Field(field) => match field {
                Field::Open => bar.open,
                Field::High => bar.high,
                Field::Low => bar.low,
                Field::Close => bar.close,
                Field::Volume => bar.volume,
            },
            Self::Engine(indicator, of) => {
                let value = of.next(bar);
                if value.is_nan() {
                    return f64::NAN;
                }
                indicator.update(value, bar).unwrap_or(f64::NAN)
            }
            Self::Window {
                kind,
                of,
                window,
                size,
            } => {
                let value = of.next(bar);
                if value.is_nan() {
                    return f64::NAN;
                }
                window.push_back(value);
                if window.len() > *size {
                    window.pop_front();
                }
                if window.len() < *size {
                    return f64::NAN;
                }
                windowed(*kind, window)
            }
            Self::Ago { of, window, bars } => {
                window.push_back(of.next(bar));
                if window.len() > *bars + 1 {
                    window.pop_front();
                }
                if window.len() == *bars + 1 {
                    window[0]
                } else {
                    f64::NAN
                }
            }
            Self::Arithmetic(arithmetic, a, b) => {
                let (a, b) = (a.next(bar), b.next(bar));
                match arithmetic {
                    Arithmetic::Add => a + b,
                    Arithmetic::Sub => a - b,
                    Arithmetic::Mul => a * b,
                    Arithmetic::Div if b == 0.0 => f64::NAN,
                    Arithmetic::Div => a / b,
                }
            }
        }
    }
}

fn windowed(kind: IndicatorKind, window: &VecDeque<f64>) -> f64 {
    match kind {
        IndicatorKind::Highest => window.iter().copied().fold(f64::MIN, f64::max),
        IndicatorKind::Lowest => window.iter().copied().fold(f64::MAX, f64::min),
        // As a percentage.
        IndicatorKind::Roc => {
            let (first, last) = (window[0], window[window.len() - 1]);
            if first == 0.0 {
                f64::NAN
            } else {
                (last / first - 1.0) * 100.0
            }
        }
        IndicatorKind::Zscore => {
            let count = window.len() as f64;
            let mean = window.iter().sum::<f64>() / count;
            let variance = window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / count;
            if variance == 0.0 {
                f64::NAN
            } else {
                (window[window.len() - 1] - mean) / variance.sqrt()
            }
        }
        IndicatorKind::Sma | IndicatorKind::Ema | IndicatorKind::Rsi | IndicatorKind::Atr => {
            f64::NAN
        }
    }
}

/// A rule being worked out bar by bar.
enum LiveRule {
    Compare {
        how: Comparison,
        a: LiveExpr,
        b: LiveExpr,
        was_above: Option<bool>,
    },
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

impl LiveRule {
    fn new(rule: &Rule, values: &BTreeMap<&str, f64>) -> Result<Self, String> {
        let all = |rules: &[Rule]| {
            rules
                .iter()
                .map(|rule| Self::new(rule, values))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(match rule {
            Rule::Compare(how, a, b) => Self::Compare {
                how: *how,
                a: LiveExpr::new(a, values)?,
                b: LiveExpr::new(b, values)?,
                was_above: None,
            },
            Rule::And(rules) => Self::And(all(rules)?),
            Rule::Or(rules) => Self::Or(all(rules)?),
            Rule::Not(rule) => Self::Not(Box::new(Self::new(rule, values)?)),
        })
    }

    /// Whether it holds at this bar. Every part always runs, as each keeps its own history.
    fn next(&mut self, bar: &Candle) -> bool {
        match self {
            Self::Compare {
                how,
                a,
                b,
                was_above,
            } => {
                let (a, b) = (a.next(bar), b.next(bar));
                if a.is_nan() || b.is_nan() {
                    return false;
                }
                let above = a > b;
                let before = was_above.replace(above);
                match how {
                    Comparison::Gt => a > b,
                    Comparison::Lt => a < b,
                    Comparison::Ge => a >= b,
                    Comparison::Le => a <= b,
                    Comparison::CrossesAbove => before == Some(false) && above,
                    Comparison::CrossesBelow => before == Some(true) && !above,
                }
            }
            // Not `all` and `any`: they'd stop early, and every part must see every bar.
            Self::And(rules) => {
                let mut holds = true;
                for rule in rules {
                    holds &= rule.next(bar);
                }
                holds
            }
            Self::Or(rules) => {
                let mut holds = false;
                for rule in rules {
                    holds |= rule.next(bar);
                }
                holds
            }
            Self::Not(rule) => !rule.next(bar),
        }
    }
}

struct Running {
    long: Option<LiveRule>,
    short: Option<LiveRule>,
    flat: Option<LiveRule>,
}

impl Signal for Running {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        let holds = |rule: &mut Option<LiveRule>| rule.as_mut().is_some_and(|r| r.next(bar));
        let (long, short, flat) = (
            holds(&mut self.long),
            holds(&mut self.short),
            holds(&mut self.flat),
        );
        Ok(match (long, short) {
            (true, false) => Some(1),
            (false, true) => Some(-1),
            _ if flat => Some(0),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use persephone_core::{
        Timeframe,
        indicator::{BuiltIn, IndicatorKind as Kind, IndicatorSpec},
    };
    use persephone_engine::indicators::compute;
    use serde_json::json;

    use super::*;

    fn bars(count: usize) -> Vec<Candle> {
        (0..count)
            .map(|i| {
                let mid = 100.0 + 10.0 * (i as f64 / 7.0).sin() + (i as f64 / 3.0).cos();
                Candle {
                    ts: i as u64 * Timeframe::H1.nanos(),
                    open: mid - 0.5,
                    high: mid + 1.0,
                    low: mid - 1.0,
                    close: mid + 0.5,
                    volume: 10.0 + (i % 5) as f64,
                }
            })
            .collect()
    }

    fn values(expr: &Value, bars: &[Candle]) -> Vec<f64> {
        let mut parser = Parser {
            params: &BTreeMap::new(),
            lengths: BTreeSet::new(),
            nodes: 0,
        };
        let parsed = parser.expr(expr, "test").unwrap();
        let mut live = LiveExpr::new(&parsed, &BTreeMap::new()).unwrap();
        bars.iter().map(|bar| live.next(bar)).collect()
    }

    fn same(a: &[f64], b: &[f64]) {
        assert_eq!(a.len(), b.len());
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
            assert!(
                (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-9,
                "bar {i}: {a} against {b}"
            );
        }
    }

    #[test]
    fn indicators_match_the_ones_charts_and_built_in_strategies_use() {
        let bars = bars(200);
        for (name, kind, period) in [
            ("sma", BuiltIn::Sma, 10),
            ("ema", BuiltIn::Ema, 12),
            ("rsi", BuiltIn::Rsi, 14),
            ("atr", BuiltIn::Atr, 9),
        ] {
            let info = Arc::new(kind.info());
            let spec = IndicatorSpec {
                kind: Kind::BuiltIn(kind),
                params: vec![f64::from(period)],
                info,
            };
            let batch = compute(&spec, &bars).unwrap();
            let live = values(&json!({ name: { "length": period } }), &bars);
            same(&live, &batch[0]);
        }
    }

    #[test]
    fn hand_written_indicators_give_the_right_numbers() {
        let bars: Vec<Candle> = [1.0, 3.0, 2.0, 5.0, 4.0]
            .iter()
            .map(|&close| Candle {
                ts: 0,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
            })
            .collect();
        let check = |expr: Value, expected: [f64; 5]| same(&values(&expr, &bars), &expected);
        let nan = f64::NAN;
        check(json!({"highest": {"length": 2}}), [nan, 3.0, 3.0, 5.0, 5.0]);
        check(json!({"lowest": {"length": 2}}), [nan, 1.0, 2.0, 2.0, 4.0]);
        check(
            json!({"roc": {"length": 1}}),
            [nan, 200.0, -100.0 / 3.0, 150.0, -20.0],
        );
        check(
            json!({"ago": {"of": "close", "bars": 2}}),
            [nan, nan, 1.0, 3.0, 2.0],
        );
        check(
            json!({"ago": {"of": "close", "bars": 0}}),
            [1.0, 3.0, 2.0, 5.0, 4.0],
        );
        check(json!({"sub": ["close", 1]}), [0.0, 2.0, 1.0, 4.0, 3.0]);
        check(json!({"div": ["close", 0]}), [nan; 5]);
        // Two values have a z-score of exactly one either side.
        check(
            json!({"zscore": {"length": 2}}),
            [nan, 1.0, -1.0, 1.0, -1.0],
        );
    }

    #[test]
    fn indicators_can_run_over_other_values() {
        let bars = bars(60);
        let of_high = values(&json!({"sma": {"of": "high", "length": 5}}), &bars);
        let of_close = values(&json!({"sma": {"length": 5}}), &bars);
        // These bars' highs are half a point over their closes.
        assert!((of_high[20] - of_close[20] - 0.5).abs() < 1e-9);
        // Waiting for what it reads: an EMA of a 5-bar SMA is ready later than either alone.
        let nested = values(
            &json!({"ema": {"of": {"sma": {"length": 5}}, "length": 3}}),
            &bars,
        );
        assert!(nested[5].is_nan() && !nested[8].is_nan());
    }

    fn cross(fast: f64, slow: f64) -> Value {
        json!({
            "params": {"fast": fast, "slow": slow},
            "long": {"crosses_above": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
            "short": {"crosses_below": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
        })
    }

    #[test]
    fn a_later_bar_never_changes_an_earlier_signal() {
        let logic = Logic::parse(&cross(3.0, 8.0)).unwrap();
        let signals = |bars: &[Candle]| {
            let mut signal = logic.create(&[3.0, 8.0]).unwrap();
            bars.iter()
                .map(|bar| signal.on_bar(bar).unwrap())
                .collect::<Vec<_>>()
        };
        let original = bars(120);
        let mut changed = original.clone();
        for bar in &mut changed[80..] {
            bar.close *= 5.0;
            bar.high *= 5.0;
        }
        assert_eq!(signals(&original)[..80], signals(&changed)[..80]);
        assert!(signals(&original).iter().flatten().count() > 2, "it trades");
    }

    #[test]
    fn rules_combine_and_keep_their_history_when_cut_short() {
        let bars = bars(100);
        // The crossing is inside an `and` whose first part is often false; it must still see
        // every bar, or it would miss what it was crossing from.
        let tree = json!({
            "long": {"and": [
                {"gt": ["close", 0]},
                {"crosses_above": ["close", {"sma": {"length": 5}}]},
            ]},
        });
        let alone = json!({"long": {"crosses_above": ["close", {"sma": {"length": 5}}]}});
        let run = |tree: &Value| {
            let mut signal = Logic::parse(tree).unwrap().create(&[]).unwrap();
            bars.iter()
                .map(|bar| signal.on_bar(bar).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(run(&tree), run(&alone));
        let never = json!({"long": {"not": {"gt": ["close", -1]}}});
        assert!(run(&never).iter().all(Option::is_none));
    }

    #[test]
    fn flat_closes_only_when_neither_side_is_asked_for() {
        let bars = bars(3);
        let run = |tree: Value| {
            let mut signal = Logic::parse(&tree).unwrap().create(&[]).unwrap();
            signal.on_bar(&bars[0]).unwrap()
        };
        let yes = json!({"gt": ["close", 0]});
        let no = json!({"lt": ["close", 0]});
        assert_eq!(run(json!({"long": yes, "flat": yes})), Some(1));
        assert_eq!(run(json!({"short": yes, "flat": yes})), Some(-1));
        assert_eq!(run(json!({"long": no, "short": no, "flat": yes})), Some(0));
        assert_eq!(run(json!({"long": yes, "short": yes})), None);
        assert_eq!(run(json!({"long": no})), None);
    }

    #[test]
    fn bad_trees_say_where_and_what_would_be_right() {
        let error = |tree: Value| Logic::parse(&tree).unwrap_err();
        assert!(error(json!({"flat": {"gt": [1, 2]}})).contains("needs a `long` or `short`"));
        let typo = error(json!({"long": {"gt": [{"smaa": {"length": 3}}, 1]}}));
        assert!(typo.starts_with("long.gt[0]: `smaa`"), "{typo}");
        assert!(typo.contains("sma"), "{typo}");
        let rule = error(json!({"long": {"and": [{"gt": [1, 2]}, {"above": [1, 2]}]}}));
        assert!(rule.starts_with("long.and[1]: `above`"), "{rule}");
        let param = error(json!({"params": {"a": 1}, "long": {"gt": ["$b", 1]}}));
        assert!(param.contains("`$b` isn't in `params`"), "{param}");
        assert!(param.contains("a"), "{param}");
        let field = error(json!({"long": {"gt": ["closed", 1]}}));
        assert!(field.contains("`closed` isn't a value"), "{field}");
        let length = error(json!({"long": {"gt": [{"sma": {"length": 2.5}}, 1]}}));
        assert!(length.contains("whole number"), "{length}");
        let param_length = error(json!({
            "params": {"n": 2.5},
            "long": {"gt": [{"sma": {"length": "$n"}}, 1]},
        }));
        assert!(param_length.contains("params.n"), "{param_length}");
        assert!(error(json!({"long": {"gt": [1]}})).contains("list of two"));
        assert!(error(json!({"long": {"gt": [1, 2], "lt": [1, 2]}})).contains("one key"));
        assert!(error(json!({"longg": {"gt": [1, 2]}})).contains("`longg`"));
        assert!(
            error(json!({"long": {"gt": [{"atr": {"of": "low", "length": 3}}, 1]}}))
                .contains("`of`")
        );
    }

    #[test]
    fn huge_trees_are_refused() {
        let mut rule = json!({"gt": [1, 2]});
        for _ in 0..MAX_NODES {
            rule = json!({"not": rule});
        }
        assert!(
            Logic::parse(&json!({ "long": rule }))
                .unwrap_err()
                .contains("nodes")
        );
    }

    #[test]
    fn a_trees_parameters_work_like_any_strategys() {
        let logic = Logic::parse(&cross(3.0, 8.0)).unwrap();
        let spec = logic.spec();
        // Its own, sorted, then the position size every strategy has.
        let names: Vec<&str> = spec.info.params.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names[..2], ["fast", "slow"]);
        assert_eq!(names.len(), 3);
        assert_eq!(spec.params[..2], [3.0, 8.0]);
        assert!(spec.info.params[0].integer, "a look-back");
        assert!(spec.info.params[0].check(2.5).is_err());

        // The same tree is always the same id, and another isn't.
        assert_eq!(logic.id(), Logic::parse(&cross(3.0, 8.0)).unwrap().id());
        assert_ne!(logic.id(), Logic::parse(&cross(3.0, 9.0)).unwrap().id());
        assert_eq!(Logic::parse(logic.source()).unwrap(), logic);
    }

    #[test]
    fn a_tree_matching_ema_cross_makes_the_same_trades_as_the_built_in() {
        use persephone_core::{InstrumentSpec, strategy::BuiltInStrategy};
        use persephone_engine::{
            BacktestRequest, FeeRates, SimSettings, TradingRules, run_backtest,
        };

        let candles: Vec<Candle> = (0..800)
            .map(|i| {
                let mid = 50_000.0 + 2_000.0 * (f64::from(i) / 40.0).sin();
                Candle {
                    ts: 1_700_000_000_000_000_000
                        + u64::try_from(i).unwrap() * Timeframe::H1.nanos(),
                    open: mid - 5.0,
                    high: mid + 20.0,
                    low: mid - 20.0,
                    close: mid + 5.0,
                    volume: 10.0,
                }
            })
            .collect();
        let request =
            |strategy: StrategySpec, plugin: Option<Arc<dyn SignalFactory>>| BacktestRequest {
                instrument: InstrumentSpec {
                    symbol: "BTCUSDT".into(),
                    venue: "BINANCE".into(),
                    base: "BTC".into(),
                    quote: "USDT".into(),
                    price_precision: 2,
                    size_precision: 5,
                },
                bars: Timeframe::H1.into(),
                candles: candles.clone(),
                strategy,
                plugin,
                ticks: None,
                starting_balance: 100_000.0,
                fees: FeeRates::ZERO,
                sim: SimSettings::default(),
                rules: TradingRules::default(),
                trade_from: 0,
                fill_bars: None,
            };
        let logic = Arc::new(Logic::parse(&cross(5.0, 20.0)).unwrap());
        let mut spec = logic.spec();
        spec.set_param(2, 10.0).unwrap();
        let plugin: Arc<dyn SignalFactory> = logic;
        let run = |request: BacktestRequest| {
            std::thread::spawn(move || run_backtest(&request).unwrap())
                .join()
                .unwrap()
        };
        let ours = run(request(spec, Some(plugin)));
        let theirs = run(request(
            StrategySpec::with_params(BuiltInStrategy::EmaCross, &[5.0, 20.0, 10.0]),
            None,
        ));
        assert!(theirs.fills.len() >= 6, "it trades");
        assert_eq!(ours.fills, theirs.fills);
        assert_eq!(ours.equity, theirs.equity);
    }
}

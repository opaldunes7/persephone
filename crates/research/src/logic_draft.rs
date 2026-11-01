//! A logic tree being built: the same tree as [`Logic`] reads, but open to edit, one change at a
//! time, and written back as the JSON it came from. The app's Build tab edits one; whatever it
//! writes is checked by [`Logic::parse`], so it can't hold a tree that wouldn't run.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::logic::{FIELDS, INDICATORS, Logic};

/// Which of a tree's three wishes a rule is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wish {
    Long,
    Short,
    Flat,
}

impl Wish {
    pub const ALL: [Self; 3] = [Self::Long, Self::Short, Self::Flat];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Flat => "flat",
        }
    }
}

/// How two values are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Gt,
    Lt,
    Ge,
    Le,
    CrossesAbove,
    CrossesBelow,
}

impl Comparison {
    pub const ALL: [Self; 6] = [
        Self::Gt,
        Self::Lt,
        Self::Ge,
        Self::Le,
        Self::CrossesAbove,
        Self::CrossesBelow,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Gt => "gt",
            Self::Lt => "lt",
            Self::Ge => "ge",
            Self::Le => "le",
            Self::CrossesAbove => "crosses_above",
            Self::CrossesBelow => "crosses_below",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => "≥",
            Self::Le => "≤",
            Self::CrossesAbove => "crosses above",
            Self::CrossesBelow => "crosses below",
        }
    }

    fn of(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }
}

impl std::fmt::Display for Comparison {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A yes or no on each bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    /// Every one of these (`and`).
    All(Vec<Rule>),
    /// Any one of these (`or`).
    Any(Vec<Rule>),
    Not(Box<Rule>),
    Compare {
        op: Comparison,
        left: Operand,
        right: Operand,
    },
}

/// A look-back: a whole number of bars, or a variable.
#[derive(Clone, Debug, PartialEq)]
pub enum Amount {
    Whole(u32),
    Param(String),
}

/// A value on each bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Number(f64),
    Param(String),
    Field(&'static str),
    /// `of` left out reads the close, as the tree does.
    Indicator {
        kind: &'static str,
        of: Option<Box<Operand>>,
        length: Amount,
    },
    Ago {
        of: Box<Operand>,
        bars: Amount,
    },
    /// `add`, `sub`, `mul`, `div`, `min` or `max` of two values.
    Arith {
        op: &'static str,
        a: Box<Operand>,
        b: Box<Operand>,
    },
    Abs(Box<Operand>),
    /// A value on a longer timeframe, seen once that bar has closed.
    On {
        timeframe: String,
        value: Box<Operand>,
    },
}

/// What an operand can be, as the builder lists them.
pub const NUMBER: &str = "number";
pub const VARIABLE: &str = "variable";
pub const ARITHMETIC: [&str; 6] = ["add", "sub", "mul", "div", "min", "max"];

/// Every kind of operand, in the order the builder lists them.
#[must_use]
pub fn operand_kinds() -> Vec<&'static str> {
    let mut kinds = vec![NUMBER, VARIABLE];
    kinds.extend(FIELDS);
    kinds.extend(INDICATORS);
    kinds.push("ago");
    kinds.extend(ARITHMETIC);
    kinds.extend(["abs", "on"]);
    kinds
}

impl Operand {
    /// Its kind, as [`operand_kinds`] names it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Number(_) => NUMBER,
            Self::Param(_) => VARIABLE,
            Self::Field(field) => field,
            Self::Indicator { kind, .. } => kind,
            Self::Ago { .. } => "ago",
            Self::Arith { op, .. } => op,
            Self::Abs(_) => "abs",
            Self::On { .. } => "on",
        }
    }

    /// A new operand of `kind`, keeping what it can of `was`: an indicator keeps its look-back,
    /// and an operation takes `was` as what it works on.
    #[must_use]
    pub fn of_kind(kind: &str, was: &Self) -> Option<Self> {
        let inner = || Box::new(was.clone());
        let length = match was {
            Self::Indicator { length, .. } => length.clone(),
            _ => Amount::Whole(14),
        };
        Some(match kind {
            NUMBER => Self::Number(match was {
                Self::Number(n) => *n,
                _ => 0.0,
            }),
            // A variable needs a name, which the draft gives it.
            VARIABLE => return None,
            "atr" => Self::Indicator {
                kind: "atr",
                of: None,
                length,
            },
            "ago" => Self::Ago {
                of: inner(),
                bars: Amount::Whole(1),
            },
            "abs" => Self::Abs(inner()),
            "on" => Self::On {
                timeframe: "4h".into(),
                value: inner(),
            },
            other => {
                if let Some(field) = FIELDS.iter().find(|f| **f == other) {
                    Self::Field(field)
                } else if let Some(indicator) = INDICATORS.iter().find(|i| **i == other) {
                    Self::Indicator {
                        kind: indicator,
                        of: match was {
                            Self::Indicator { of, .. } => of.clone(),
                            _ => None,
                        },
                        length,
                    }
                } else {
                    let op = ARITHMETIC.iter().find(|a| **a == other)?;
                    Self::Arith {
                        op,
                        a: inner(),
                        b: Box::new(Self::Number(1.0)),
                    }
                }
            }
        })
    }

    fn children(&self) -> Vec<&Self> {
        match self {
            Self::Indicator { of, .. } => of.iter().map(AsRef::as_ref).collect(),
            Self::Ago { of, .. } => vec![of],
            Self::Arith { a, b, .. } => vec![a, b],
            Self::Abs(value) | Self::On { value, .. } => vec![value],
            _ => Vec::new(),
        }
    }

    fn child_mut(&mut self, at: u8) -> Option<&mut Self> {
        match (self, at) {
            (Self::Indicator { of, kind, .. }, 0) => {
                // Reading the close is the default; asking for it in the builder writes it out.
                if *kind == "atr" {
                    return None;
                }
                Some(of.get_or_insert_with(|| Box::new(Self::Field("close"))))
            }
            (Self::Ago { of, .. }, 0)
            | (Self::Abs(of), 0)
            | (Self::On { value: of, .. }, 0)
            | (Self::Arith { a: of, .. }, 0)
            | (Self::Arith { b: of, .. }, 1) => Some(of),
            _ => None,
        }
    }

    fn amount_mut(&mut self) -> Option<&mut Amount> {
        match self {
            Self::Indicator { length, .. } => Some(length),
            Self::Ago { bars, .. } => Some(bars),
            _ => None,
        }
    }

    fn params_used(&self, into: &mut Vec<String>) {
        match self {
            Self::Param(name) => into.push(name.clone()),
            Self::Indicator {
                length: Amount::Param(name),
                ..
            }
            | Self::Ago {
                bars: Amount::Param(name),
                ..
            } => into.push(name.clone()),
            _ => {}
        }
        for child in self.children() {
            child.params_used(into);
        }
    }

    fn rename(&mut self, from: &str, to: &str) {
        match self {
            Self::Param(name) if name == from => *name = to.to_owned(),
            _ => {}
        }
        if let Some(Amount::Param(name)) = self.amount_mut()
            && name == from
        {
            *name = to.to_owned();
        }
        for at in 0..2 {
            if let Some(child) = self.child_mut_existing(at) {
                child.rename(from, to);
            }
        }
    }

    /// A child that's already there: unlike [`Self::child_mut`], never writes out the close.
    fn child_mut_existing(&mut self, at: u8) -> Option<&mut Self> {
        match (self, at) {
            (Self::Indicator { of: Some(of), .. }, 0)
            | (Self::Ago { of, .. }, 0)
            | (Self::Abs(of), 0)
            | (Self::On { value: of, .. }, 0)
            | (Self::Arith { a: of, .. }, 0)
            | (Self::Arith { b: of, .. }, 1) => Some(of),
            _ => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Number(n) => number(*n),
            Self::Param(name) => json!(format!("${name}")),
            Self::Field(field) => json!(field),
            Self::Indicator { kind, of, length } => {
                let mut inner = Map::new();
                if let Some(of) = of {
                    inner.insert("of".into(), of.to_json());
                }
                inner.insert("length".into(), length.to_json());
                json!({ *kind: inner })
            }
            Self::Ago { of, bars } => json!({"ago": {"of": of.to_json(), "bars": bars.to_json()}}),
            Self::Arith { op, a, b } => json!({ *op: [a.to_json(), b.to_json()] }),
            Self::Abs(value) => json!({"abs": value.to_json()}),
            Self::On { timeframe, value } => {
                json!({"on": {"timeframe": timeframe, "value": value.to_json()}})
            }
        }
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(Self::Number(n.as_f64().unwrap_or(0.0))),
            Value::String(text) => match text.strip_prefix('$') {
                Some(name) => Ok(Self::Param(name.to_owned())),
                None => FIELDS
                    .iter()
                    .find(|f| **f == text)
                    .map(|f| Self::Field(f))
                    .ok_or_else(|| format!("`{text}` isn't a value")),
            },
            Value::Object(object) => {
                let (key, inner) = object.iter().next().ok_or("an empty value")?;
                if let Some(kind) = INDICATORS.iter().find(|i| **i == key) {
                    return Ok(Self::Indicator {
                        kind,
                        of: inner
                            .get("of")
                            .map(Self::from_json)
                            .transpose()?
                            .map(Box::new),
                        length: Amount::from_json(&inner["length"])?,
                    });
                }
                if let Some(op) = ARITHMETIC.iter().find(|a| **a == key) {
                    return Ok(Self::Arith {
                        op,
                        a: Box::new(Self::from_json(&inner[0])?),
                        b: Box::new(Self::from_json(&inner[1])?),
                    });
                }
                match key.as_str() {
                    "ago" => Ok(Self::Ago {
                        of: Box::new(Self::from_json(&inner["of"])?),
                        bars: Amount::from_json(&inner["bars"])?,
                    }),
                    "abs" => Ok(Self::Abs(Box::new(Self::from_json(inner)?))),
                    "on" => Ok(Self::On {
                        timeframe: inner["timeframe"].as_str().unwrap_or("4h").to_owned(),
                        value: Box::new(Self::from_json(&inner["value"])?),
                    }),
                    other => Err(format!("`{other}` isn't a value")),
                }
            }
            other => Err(format!("{other} isn't a value")),
        }
    }
}

impl Amount {
    fn to_json(&self) -> Value {
        match self {
            Self::Whole(n) => json!(n),
            Self::Param(name) => json!(format!("${name}")),
        }
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        if let Some(name) = value.as_str().and_then(|t| t.strip_prefix('$')) {
            return Ok(Self::Param(name.to_owned()));
        }
        value
            .as_f64()
            .filter(|n| n.fract() == 0.0 && *n >= 0.0)
            .map(|n| Self::Whole(n as u32))
            .ok_or_else(|| format!("{value} isn't a whole number"))
    }
}

/// A whole number written as one, so a tree read and written again is the same tree.
fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

impl Rule {
    /// A comparison to start from: the close above its 20-bar average.
    #[must_use]
    pub fn starter() -> Self {
        Self::Compare {
            op: Comparison::Gt,
            left: Operand::Field("close"),
            right: Operand::Indicator {
                kind: "sma",
                of: None,
                length: Amount::Whole(20),
            },
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::All(rules) => json!({"and": rules.iter().map(Self::to_json).collect::<Vec<_>>()}),
            Self::Any(rules) => json!({"or": rules.iter().map(Self::to_json).collect::<Vec<_>>()}),
            Self::Not(rule) => json!({"not": rule.to_json()}),
            Self::Compare { op, left, right } => {
                json!({ op.key(): [left.to_json(), right.to_json()] })
            }
        }
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        let (key, inner) = value
            .as_object()
            .and_then(|o| o.iter().next())
            .ok_or_else(|| format!("{value} isn't a rule"))?;
        if let Some(op) = Comparison::of(key) {
            return Ok(Self::Compare {
                op,
                left: Operand::from_json(&inner[0])?,
                right: Operand::from_json(&inner[1])?,
            });
        }
        let list = || -> Result<Vec<Self>, String> {
            inner
                .as_array()
                .ok_or("expected a list of rules")?
                .iter()
                .map(Self::from_json)
                .collect()
        };
        match key.as_str() {
            "and" => Ok(Self::All(list()?)),
            "or" => Ok(Self::Any(list()?)),
            "not" => Ok(Self::Not(Box::new(Self::from_json(inner)?))),
            other => Err(format!("`{other}` isn't a rule")),
        }
    }

    fn operands(&self) -> Vec<&Operand> {
        match self {
            Self::All(rules) | Self::Any(rules) => rules.iter().flat_map(Self::operands).collect(),
            Self::Not(rule) => rule.operands(),
            Self::Compare { left, right, .. } => vec![left, right],
        }
    }

    fn rule_mut(&mut self, at: u8) -> Option<&mut Self> {
        match self {
            Self::All(rules) | Self::Any(rules) => rules.get_mut(usize::from(at)),
            Self::Not(rule) if at == 0 => Some(rule),
            _ => None,
        }
    }
}

/// Where in a draft a rule or operand is: which wish, then each step in. Inside a group a step is
/// the rule's place; inside `not`, 0; at a comparison 0 is the left value and 1 the right; inside an
/// operand, 0 is what it works on and 1 an arithmetic's second value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodePath {
    pub wish: Wish,
    pub steps: Vec<u8>,
}

impl NodePath {
    #[must_use]
    pub fn root(wish: Wish) -> Self {
        Self {
            wish,
            steps: Vec::new(),
        }
    }

    #[must_use]
    pub fn then(&self, step: u8) -> Self {
        let mut steps = self.steps.clone();
        steps.push(step);
        Self {
            wish: self.wish,
            steps,
        }
    }
}

/// A tree being built.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub name: String,
    /// Each variable and the value it starts at.
    pub params: BTreeMap<String, f64>,
    pub long: Option<Rule>,
    pub short: Option<Rule>,
    pub flat: Option<Rule>,
}

impl Default for Draft {
    /// Long while the close is over its 20-bar average, out when it falls under.
    fn default() -> Self {
        Self {
            name: "My strategy".into(),
            params: BTreeMap::new(),
            long: Some(Rule::All(vec![Rule::starter()])),
            short: None,
            flat: Some(Rule::All(vec![Rule::Compare {
                op: Comparison::Lt,
                left: Operand::Field("close"),
                right: Operand::Indicator {
                    kind: "sma",
                    of: None,
                    length: Amount::Whole(20),
                },
            }])),
        }
    }
}

/// The guide's examples, to start from.
pub const EXAMPLES: [(&str, &str); 3] = [
    (
        "Fast EMA over slow",
        r#"{"name":"Fast over slow","params":{"fast":12,"slow":26},
            "long":{"crosses_above":[{"ema":{"length":"$fast"}},{"ema":{"length":"$slow"}}]},
            "short":{"crosses_below":[{"ema":{"length":"$fast"}},{"ema":{"length":"$slow"}}]}}"#,
    ),
    (
        "Above its average, in rising volume",
        r#"{"name":"Above its average, in rising volume","params":{"length":20},
            "long":{"and":[{"gt":["close",{"sma":{"length":"$length"}}]},
                           {"gt":["volume",{"ago":{"of":"volume","bars":1}}]}]},
            "flat":{"lt":["close",{"sma":{"length":"$length"}}]}}"#,
    ),
    (
        "Breakout with a stop, out after two days, weekdays only",
        r#"{"name":"Breakout with a stop, out after two days, weekdays only",
            "params":{"stop":0.97,"most_bars":48},
            "long":{"and":[{"crosses_above":["close",{"ago":{"of":{"highest":{"length":50}},"bars":1}}]},
                           {"lt":["weekday",5]}]},
            "flat":{"or":[{"lt":["close",{"mul":["entry_price","$stop"]}]},
                          {"ge":["bars_held","$most_bars"]}]}}"#,
    ),
];

impl Draft {
    /// Reads a tree, or one as a strategy is written with it (`{"logic": …, "params": …}`). It's
    /// checked as [`Logic::parse`] checks it first, so the error says where it's wrong.
    ///
    /// # Errors
    ///
    /// Returns [`Logic::parse`]'s message.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let tree = value.get("logic").unwrap_or(value);
        Logic::parse(tree)?;
        let side = |key: &str| tree.get(key).map(Rule::from_json).transpose();
        Ok(Self {
            name: tree
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Logic tree")
                .to_owned(),
            params: tree
                .get("params")
                .and_then(Value::as_object)
                .map(|params| {
                    params
                        .iter()
                        .filter_map(|(name, v)| Some((name.clone(), v.as_f64()?)))
                        .collect()
                })
                .unwrap_or_default(),
            long: side("long")?,
            short: side("short")?,
            flat: side("flat")?,
        })
    }

    /// The tree, as [`Logic::parse`] reads it.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut tree = Map::new();
        tree.insert("name".into(), json!(self.name));
        if !self.params.is_empty() {
            tree.insert(
                "params".into(),
                Value::Object(
                    self.params
                        .iter()
                        .map(|(name, value)| (name.clone(), number(*value)))
                        .collect(),
                ),
            );
        }
        for wish in Wish::ALL {
            if let Some(rule) = self.side(wish) {
                tree.insert(wish.key().into(), rule.to_json());
            }
        }
        Value::Object(tree)
    }

    /// The tree checked, ready to run.
    ///
    /// # Errors
    ///
    /// Returns where it's wrong.
    pub fn logic(&self) -> Result<Logic, String> {
        Logic::parse(&self.to_json())
    }

    #[must_use]
    pub const fn side(&self, wish: Wish) -> Option<&Rule> {
        match wish {
            Wish::Long => self.long.as_ref(),
            Wish::Short => self.short.as_ref(),
            Wish::Flat => self.flat.as_ref(),
        }
    }

    pub const fn side_mut(&mut self, wish: Wish) -> &mut Option<Rule> {
        match wish {
            Wish::Long => &mut self.long,
            Wish::Short => &mut self.short,
            Wish::Flat => &mut self.flat,
        }
    }

    /// The rule at `path`, which ends on a rule.
    pub fn rule_mut(&mut self, path: &NodePath) -> Option<&mut Rule> {
        let mut rule = self.side_mut(path.wish).as_mut()?;
        for &step in &path.steps {
            rule = rule.rule_mut(step)?;
        }
        Some(rule)
    }

    #[must_use]
    pub fn rule(&self, path: &NodePath) -> Option<&Rule> {
        let mut rule = self.side(path.wish)?;
        for &step in &path.steps {
            rule = match rule {
                Rule::All(rules) | Rule::Any(rules) => rules.get(usize::from(step))?,
                Rule::Not(inner) if step == 0 => inner,
                _ => return None,
            };
        }
        Some(rule)
    }

    /// The operand at `path`: steps to a comparison, then 0 or 1 for its side, then steps in.
    pub fn operand_mut(&mut self, path: &NodePath) -> Option<&mut Operand> {
        let mut rule = self.side_mut(path.wish).as_mut()?;
        let mut steps = path.steps.iter().copied();
        let mut operand = loop {
            let step = steps.next()?;
            match rule {
                Rule::Compare { left, right, .. } => {
                    break match step {
                        0 => left,
                        1 => right,
                        _ => return None,
                    };
                }
                other => rule = other.rule_mut(step)?,
            }
        };
        for step in steps {
            operand = operand.child_mut(step)?;
        }
        Some(operand)
    }

    /// Starts `wish` with one condition, or takes it away.
    pub fn set_side(&mut self, wish: Wish, on: bool) {
        *self.side_mut(wish) = on.then(|| Rule::All(vec![Rule::starter()]));
        self.prune();
    }

    /// Adds a condition, or a group of one, to the group at `path`.
    pub fn add(&mut self, path: &NodePath, group: bool) -> bool {
        let new = if group {
            Rule::Any(vec![Rule::starter()])
        } else {
            Rule::starter()
        };
        match self.rule_mut(path) {
            Some(Rule::All(rules) | Rule::Any(rules)) => {
                rules.push(new);
                true
            }
            _ => false,
        }
    }

    /// Takes away the rule at `path`. A group left empty goes too, and a wish left with nothing
    /// is turned off.
    pub fn remove(&mut self, path: &NodePath) -> bool {
        let Some((&last, parent)) = path.steps.split_last() else {
            *self.side_mut(path.wish) = None;
            self.prune();
            return true;
        };
        let parent = NodePath {
            wish: path.wish,
            steps: parent.to_vec(),
        };
        let emptied = match self.rule_mut(&parent) {
            Some(Rule::All(rules) | Rule::Any(rules)) if usize::from(last) < rules.len() => {
                rules.remove(usize::from(last));
                rules.is_empty()
            }
            _ => return false,
        };
        if emptied {
            self.remove(&parent);
        }
        self.prune();
        true
    }

    /// Makes the group at `path` need every one of its rules, or any one.
    pub fn set_group(&mut self, path: &NodePath, all: bool) -> bool {
        let Some(rule) = self.rule_mut(path) else {
            return false;
        };
        let rules = match rule {
            Rule::All(rules) | Rule::Any(rules) => std::mem::take(rules),
            _ => return false,
        };
        *rule = if all {
            Rule::All(rules)
        } else {
            Rule::Any(rules)
        };
        true
    }

    /// Wraps the rule at `path` in `not`, or takes the `not` away.
    pub fn toggle_not(&mut self, path: &NodePath) -> bool {
        let Some(rule) = self.rule_mut(path) else {
            return false;
        };
        let taken = std::mem::replace(rule, Rule::All(Vec::new()));
        *rule = match taken {
            Rule::Not(inner) => *inner,
            other => Rule::Not(Box::new(other)),
        };
        true
    }

    pub fn set_comparison(&mut self, path: &NodePath, op: Comparison) -> bool {
        match self.rule_mut(path) {
            Some(Rule::Compare { op: was, .. }) => {
                *was = op;
                true
            }
            _ => false,
        }
    }

    /// Makes the operand at `path` another kind. A variable is named for where it is.
    pub fn set_kind(&mut self, path: &NodePath, kind: &str) -> bool {
        if kind == VARIABLE {
            return self.to_variable(path);
        }
        let Some(operand) = self.operand_mut(path) else {
            return false;
        };
        let Some(new) = Operand::of_kind(kind, operand) else {
            return false;
        };
        *operand = new;
        self.prune();
        true
    }

    pub fn set_number(&mut self, path: &NodePath, value: f64) -> bool {
        match self.operand_mut(path) {
            Some(Operand::Number(n)) if value.is_finite() => {
                *n = value;
                true
            }
            _ => false,
        }
    }

    pub fn set_timeframe(&mut self, path: &NodePath, timeframe: &str) -> bool {
        match self.operand_mut(path) {
            Some(Operand::On { timeframe: was, .. }) => {
                timeframe.clone_into(was);
                true
            }
            _ => false,
        }
    }

    /// Sets the look-back of the indicator or `ago` at `path`.
    pub fn set_amount(&mut self, path: &NodePath, bars: u32) -> bool {
        match self.operand_mut(path).and_then(Operand::amount_mut) {
            Some(amount @ Amount::Whole(_)) => {
                *amount = Amount::Whole(bars);
                true
            }
            _ => false,
        }
    }

    /// Turns the number at `path` into a variable starting at it, named for where it is: `$` in
    /// the builder.
    pub fn to_variable(&mut self, path: &NodePath) -> bool {
        let name = self.fresh_name(&self.name_for(path));
        let Some(operand) = self.operand_mut(path) else {
            return false;
        };
        let start = match operand {
            Operand::Number(n) => *n,
            Operand::Param(_) => return false,
            _ => 1.0,
        };
        *operand = Operand::Param(name.clone());
        self.params.insert(name, start);
        true
    }

    /// Turns the variable at `path` back into the number it starts at: `#` in the builder.
    pub fn to_constant(&mut self, path: &NodePath) -> bool {
        let start = |draft: &Self, name: &str| draft.params.get(name).copied().unwrap_or(0.0);
        let Some(Operand::Param(name)) = self.operand_mut(path).cloned() else {
            return false;
        };
        let value = start(self, &name);
        if let Some(operand) = self.operand_mut(path) {
            *operand = Operand::Number(value);
        }
        self.prune();
        true
    }

    /// Turns the look-back at `path` into a variable, or back into its number.
    pub fn toggle_amount_variable(&mut self, path: &NodePath) -> bool {
        let name = self.fresh_name(&self.name_for(path));
        let params = self.params.clone();
        let Some(amount) = self.operand_mut(path).and_then(Operand::amount_mut) else {
            return false;
        };
        match amount.clone() {
            Amount::Whole(n) => {
                *amount = Amount::Param(name.clone());
                self.params.insert(name, f64::from(n.max(1)));
            }
            Amount::Param(was) => {
                let value = params.get(&was).copied().unwrap_or(1.0).max(1.0);
                *amount = Amount::Whole(value as u32);
                self.prune();
            }
        }
        true
    }

    /// Renames a variable everywhere it's used.
    ///
    /// # Errors
    ///
    /// Returns why not: an empty name, or one already taken.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() || !to.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("A variable's name is letters, digits and _.".into());
        }
        if to == from {
            return Ok(());
        }
        if self.params.contains_key(to) {
            return Err(format!("There's already a ${to}."));
        }
        let Some(value) = self.params.remove(from) else {
            return Err(format!("There's no ${from}."));
        };
        self.params.insert(to.to_owned(), value);
        for wish in Wish::ALL {
            if let Some(rule) = self.side_mut(wish).as_mut() {
                rename_in(rule, from, to);
            }
        }
        Ok(())
    }

    /// Variables used, each once, in the order they first appear.
    #[must_use]
    pub fn used(&self) -> Vec<String> {
        let mut used = Vec::new();
        for wish in Wish::ALL {
            if let Some(rule) = self.side(wish) {
                for operand in rule.operands() {
                    operand.params_used(&mut used);
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        used.retain(|name| seen.insert(name.clone()));
        used
    }

    /// Drops variables nothing uses any more.
    fn prune(&mut self) {
        let used = self.used();
        self.params.retain(|name, _| used.contains(name));
    }

    /// A name for a variable at `path`, from what it's part of: `sma_length`, `ago_bars`, `rsi`.
    fn name_for(&self, path: &NodePath) -> String {
        let mut draft = self.clone();
        let Some(operand) = draft.operand_mut(path).cloned() else {
            return "value".into();
        };
        match &operand {
            Operand::Indicator { kind, .. } => format!("{kind}_length"),
            Operand::Ago { .. } => "ago_bars".into(),
            _ => {
                // A plain number: named for what it's compared with.
                let mut parent = path.clone();
                let side = parent.steps.pop();
                let compared = draft.rule(&parent).and_then(|rule| match rule {
                    Rule::Compare { left, right, .. } => Some(if side == Some(0) {
                        right.kind()
                    } else {
                        left.kind()
                    }),
                    _ => None,
                });
                match compared {
                    Some(kind) if kind != NUMBER && kind != VARIABLE => format!("{kind}_level"),
                    _ => "value".into(),
                }
            }
        }
    }

    fn fresh_name(&self, base: &str) -> String {
        if !self.params.contains_key(base) {
            return base.to_owned();
        }
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|name| !self.params.contains_key(name))
            .unwrap_or_else(|| base.to_owned())
    }
}

fn rename_in(rule: &mut Rule, from: &str, to: &str) {
    match rule {
        Rule::All(rules) | Rule::Any(rules) => {
            for rule in rules {
                rename_in(rule, from, to);
            }
        }
        Rule::Not(inner) => rename_in(inner, from, to),
        Rule::Compare { left, right, .. } => {
            left.rename(from, to);
            right.rename(from, to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(wish: Wish, steps: &[u8]) -> NodePath {
        NodePath {
            wish,
            steps: steps.to_vec(),
        }
    }

    #[test]
    fn the_guides_examples_read_and_write_back_the_same() {
        for (_, text) in EXAMPLES {
            let tree: Value = serde_json::from_str(text).unwrap();
            let draft = Draft::from_json(&tree).unwrap();
            assert_eq!(draft.to_json(), tree);
            assert_eq!(
                draft.logic().unwrap().id(),
                Logic::parse(&tree).unwrap().id()
            );
        }
        // With the strategy's wrapper too, as an agent writes it.
        let wrapped = json!({"logic": {"long": {"gt": ["close", 1]}}, "params": {"Size %": 50}});
        assert!(Draft::from_json(&wrapped).unwrap().long.is_some());
    }

    #[test]
    fn every_kind_of_value_round_trips() {
        let tree = json!({"name": "All kinds", "params": {"k": 2.5},
            "long": {"and": [
                {"not": {"lt": [{"abs": {"sub": ["close", "open"]}}, {"atr": {"length": 14}}]}},
                {"crosses_above": [{"on": {"timeframe": "1d", "value": {"rsi": {"of": "high", "length": 10}}}}, 50.5]},
                {"or": [{"ge": [{"zscore": {"length": 30}}, "$k"]}, {"le": ["hour", 3]}]},
            ]},
            "short": {"gt": [{"max": ["low", {"ago": {"of": "close", "bars": 0}}]}, 1]},
        });
        let draft = Draft::from_json(&tree).unwrap();
        assert_eq!(draft.to_json(), tree);
    }

    #[test]
    fn a_bad_tree_says_where() {
        let error = Draft::from_json(&json!({"long": {"and": [{"gtt": [1, 2]}]}})).unwrap_err();
        assert!(error.starts_with("long.and[0]"), "{error}");
    }

    #[test]
    fn edits_keep_it_a_tree_that_runs() {
        let mut draft = Draft::default();
        let long = NodePath::root(Wish::Long);
        // Every edit, then it's still a tree Logic reads.
        assert!(draft.add(&long, false));
        assert!(draft.set_comparison(&long.then(1), Comparison::CrossesAbove));
        assert!(draft.set_kind(&long.then(1).then(0), "ema"));
        assert!(draft.set_amount(&long.then(1).then(0), 12));
        assert!(draft.set_kind(&long.then(1).then(1), "ema"));
        assert!(draft.set_amount(&long.then(1).then(1), 26));
        assert!(draft.add(&long, true));
        assert!(draft.set_group(&long, false));
        assert!(draft.toggle_not(&long.then(0)));
        assert!(draft.set_kind(&long.then(2).then(0).then(1), "on"));
        assert!(draft.set_timeframe(&long.then(2).then(0).then(1), "1d"));
        draft.set_side(Wish::Short, true);
        draft.logic().unwrap();
        let json = draft.to_json();
        assert_eq!(
            json["long"]["or"][1]["crosses_above"][0],
            json!({"ema": {"length": 12}})
        );
        assert_eq!(
            json["long"]["or"][2]["or"][0]["gt"][1]["on"]["timeframe"],
            "1d"
        );
        assert!(json["long"]["or"][0]["not"].is_object());

        // Taking every condition of a group away takes the group; of a wish, the wish.
        assert!(draft.remove(&long.then(2).then(0)));
        assert_eq!(draft.to_json()["long"]["or"].as_array().unwrap().len(), 2);
        draft.set_side(Wish::Flat, false);
        draft.logic().unwrap();
        assert!(draft.to_json().get("flat").is_none());
        assert!(draft.remove(&NodePath::root(Wish::Short)));
        assert!(draft.short.is_none());
    }

    #[test]
    fn numbers_become_variables_named_for_where_they_are_and_back() {
        let mut draft = Draft::default();
        let average = path(Wish::Long, &[0, 1]);
        assert!(draft.toggle_amount_variable(&average));
        assert_eq!(draft.params.get("sma_length"), Some(&20.0));
        // The flat side's average gets its own name.
        assert!(draft.toggle_amount_variable(&path(Wish::Flat, &[0, 1])));
        assert_eq!(draft.params.get("sma_length_2"), Some(&20.0));
        let info = draft.logic().unwrap().info();
        let names: Vec<&str> = info.params.iter().map(|p| p.name.as_ref()).collect();
        assert!(names.contains(&"sma_length") && names.contains(&"sma_length_2"));

        // A level compared with something is named for it.
        assert!(draft.set_kind(&path(Wish::Long, &[0, 1]), NUMBER));
        assert!(draft.set_number(&path(Wish::Long, &[0, 1]), 100.0));
        assert!(draft.to_variable(&path(Wish::Long, &[0, 1])));
        assert_eq!(draft.params.get("close_level"), Some(&100.0));
        // The long side's average went, so its variable did too.
        assert!(!draft.params.contains_key("sma_length"));

        draft.rename("close_level", "entry").unwrap();
        assert_eq!(draft.to_json()["long"]["and"][0]["gt"][1], "$entry");
        assert!(draft.rename("entry", "sma_length_2").is_err());
        assert!(draft.rename("entry", "no good").is_err());

        assert!(draft.to_constant(&path(Wish::Long, &[0, 1])));
        assert_eq!(draft.to_json()["long"]["and"][0]["gt"][1], 100);
        assert!(!draft.params.contains_key("entry"));
        assert!(draft.toggle_amount_variable(&path(Wish::Flat, &[0, 1])));
        assert!(draft.params.is_empty());
        draft.logic().unwrap();
    }
}

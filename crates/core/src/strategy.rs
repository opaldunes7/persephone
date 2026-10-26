//! Strategies: the built-in catalog, and the description every strategy (built-in or plugin)
//! shares. The engine runs built-ins; plugins run in the plugin host behind a [`Signal`].
//!
//! [`Signal`]: crate::signal::Signal

use std::{borrow::Cow, fmt, sync::Arc};

use crate::indicator::Param;

/// Every strategy's last parameter: how much of the starting balance each position uses. The
/// host adds it to plugin strategies too, so sizing works the same everywhere.
pub const SIZE: Param = Param {
    name: Cow::Borrowed("Size %"),
    default: 10.0,
    min: 1.0,
    max: 100.0,
    integer: false,
};

const fn level(name: &'static str, default: f64) -> Param {
    Param {
        name: Cow::Borrowed(name),
        default,
        min: 1.0,
        max: 99.0,
        integer: false,
    }
}

/// Everything about a strategy except its state.
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyInfo {
    pub name: String,
    /// One line for menus.
    pub description: String,
    /// The strategy's own parameters followed by [`SIZE`].
    pub params: Vec<Param>,
}

impl StrategyInfo {
    /// A description from the strategy's own parameters; [`SIZE`] is appended.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        mut params: Vec<Param>,
    ) -> Self {
        params.push(SIZE);
        Self {
            name: name.into(),
            description: description.into(),
            params,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltInStrategy {
    /// Long when the fast EMA crosses above the slow one, short on the opposite cross.
    EmaCross,
    /// Long when RSI climbs back out of oversold, short when it falls back out of overbought.
    RsiReversion,
    /// Long on a close above the previous N-bar high, short below the previous N-bar low.
    DonchianBreakout,
}

impl BuiltInStrategy {
    pub const ALL: [Self; 3] = [Self::EmaCross, Self::RsiReversion, Self::DonchianBreakout];

    /// Stable identifier, used when saving layouts.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::EmaCross => "ema-cross",
            Self::RsiReversion => "rsi-reversion",
            Self::DonchianBreakout => "donchian-breakout",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EmaCross => "EMA Cross",
            Self::RsiReversion => "RSI Reversion",
            Self::DonchianBreakout => "Donchian Breakout",
        }
    }

    /// One line for menus.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::EmaCross => "Trend: follow fast/slow EMA crosses",
            Self::RsiReversion => "Mean reversion: fade RSI extremes",
            Self::DonchianBreakout => "Breakout: trade new N-bar highs and lows",
        }
    }

    /// The strategy's own parameters (without [`SIZE`]).
    #[must_use]
    pub const fn params(self) -> &'static [Param] {
        match self {
            Self::EmaCross => const { &[Param::period("Fast", 10.0), Param::period("Slow", 30.0)] },
            Self::RsiReversion => {
                const {
                    &[
                        Param::period("Length", 14.0),
                        level("Oversold", 30.0),
                        level("Overbought", 70.0),
                    ]
                }
            }
            Self::DonchianBreakout => const { &[Param::period("Length", 20.0)] },
        }
    }

    #[must_use]
    pub fn info(self) -> StrategyInfo {
        StrategyInfo::new(self.name(), self.description(), self.params().to_vec())
    }
}

/// Which strategy: a built-in, or a plugin by id (its file name without `.wasm`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StrategyKind {
    BuiltIn(BuiltInStrategy),
    Plugin(String),
}

/// One strategy on a chart: which one, its description, and its parameter values.
#[derive(Clone, Debug)]
pub struct StrategySpec {
    pub kind: StrategyKind,
    pub info: Arc<StrategyInfo>,
    /// One value per `info.params` entry; the last is the position size.
    pub params: Vec<f64>,
}

impl PartialEq for StrategySpec {
    /// Same strategy with the same values; the description follows from the kind.
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.params == other.params
    }
}

impl StrategySpec {
    /// A built-in with its default parameters.
    #[must_use]
    pub fn new(kind: BuiltInStrategy) -> Self {
        Self::from_info(StrategyKind::BuiltIn(kind), Arc::new(kind.info()), &[])
    }

    /// A built-in with saved values, falling back to defaults for missing or invalid ones.
    #[must_use]
    pub fn with_params(kind: BuiltInStrategy, params: &[f64]) -> Self {
        Self::from_info(StrategyKind::BuiltIn(kind), Arc::new(kind.info()), params)
    }

    /// Any strategy from its description, with saved values where they are still valid.
    #[must_use]
    pub fn from_info(kind: StrategyKind, info: Arc<StrategyInfo>, saved: &[f64]) -> Self {
        let mut spec = Self {
            params: info.params.iter().map(|p| p.default).collect(),
            kind,
            info,
        };
        for (i, value) in saved.iter().enumerate().take(spec.params.len()) {
            let _ = spec.set_param(i, *value);
        }
        spec
    }

    /// Sets parameter `index` if `value` is within its limits.
    ///
    /// # Errors
    ///
    /// Returns a message describing the allowed range.
    pub fn set_param(&mut self, index: usize, value: f64) -> Result<(), String> {
        let param = self
            .info
            .params
            .get(index)
            .ok_or_else(|| format!("{} has no parameter {index}", self.info.name))?;
        param.check(value)?;
        self.params[index] = value;
        Ok(())
    }

    /// Parameter `index` as a whole number (periods).
    #[must_use]
    pub fn period(&self, index: usize) -> usize {
        self.params.get(index).copied().unwrap_or(1.0).max(1.0) as usize
    }

    /// The strategy's own parameters, without the position size.
    #[must_use]
    pub fn own_params(&self) -> &[f64] {
        &self.params[..self.params.len().saturating_sub(1)]
    }

    /// Share of the starting balance each position uses, from 0.01 to 1.
    #[must_use]
    pub fn size_fraction(&self) -> f64 {
        self.params.last().copied().unwrap_or(SIZE.default) / 100.0
    }

    /// Checks rules that span parameters (fast < slow, oversold < overbought). Plugins check
    /// their own when they start.
    ///
    /// # Errors
    ///
    /// Returns a message saying which rule is broken.
    pub fn validate(&self) -> Result<(), String> {
        match self.kind {
            StrategyKind::BuiltIn(BuiltInStrategy::EmaCross)
                if self.params[0] >= self.params[1] =>
            {
                Err(format!(
                    "Fast ({}) must be shorter than Slow ({})",
                    self.params[0], self.params[1]
                ))
            }
            StrategyKind::BuiltIn(BuiltInStrategy::RsiReversion)
                if self.params[1] >= self.params[2] =>
            {
                Err(format!(
                    "Oversold ({}) must be below Overbought ({})",
                    self.params[1], self.params[2]
                ))
            }
            _ => Ok(()),
        }
    }
}

impl fmt::Display for StrategySpec {
    /// Legend label without the size, e.g. `EMA Cross 10 30`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.info.name)?;
        for value in self.own_params() {
            write!(f, " {value}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for kind in BuiltInStrategy::ALL {
            assert_eq!(BuiltInStrategy::from_id(kind.id()), Some(kind));
            let info = kind.info();
            assert_eq!(info.params.last().unwrap().name, "Size %");
            assert!(
                StrategySpec::new(kind).validate().is_ok(),
                "{kind:?} defaults are valid"
            );
            for p in &info.params {
                assert!(p.check(p.default).is_ok());
            }
        }
    }

    #[test]
    fn labels_sizes_and_validation() {
        let mut ema = StrategySpec::new(BuiltInStrategy::EmaCross);
        assert_eq!(ema.to_string(), "EMA Cross 10 30");
        assert_eq!(ema.own_params(), &[10.0, 30.0]);
        assert!((ema.size_fraction() - 0.10).abs() < 1e-12);

        ema.set_param(0, 40.0).unwrap();
        assert!(ema.validate().is_err(), "fast must be shorter than slow");
        assert!(ema.set_param(2, 0.5).is_err(), "size is at least 1%");

        let rsi =
            StrategySpec::with_params(BuiltInStrategy::RsiReversion, &[14.0, 80.0, 20.0, 5.0]);
        assert!(rsi.validate().is_err(), "oversold must be below overbought");
        assert_eq!(rsi.to_string(), "RSI Reversion 14 80 20");
    }

    #[test]
    fn plugin_strategies_get_a_size_too() {
        let info = Arc::new(StrategyInfo::new(
            "Turtle",
            "Breakouts",
            vec![Param::period("Entry", 20.0)],
        ));
        let spec =
            StrategySpec::from_info(StrategyKind::Plugin("turtle".into()), info, &[55.0, 25.0]);
        assert_eq!(spec.own_params(), &[55.0]);
        assert!((spec.size_fraction() - 0.25).abs() < 1e-12);
        assert!(spec.validate().is_ok(), "plugins check their own rules");
    }
}

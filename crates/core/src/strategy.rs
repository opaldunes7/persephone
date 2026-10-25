//! The built-in strategy catalog: names, parameters and limits. The engine runs them; WASM
//! plugin strategies (M5) will appear alongside.

use std::fmt;

use crate::indicator::Param;

/// Every strategy's last parameter: how much of the starting balance each position uses.
const SIZE: Param = Param {
    name: "Size %",
    default: 10.0,
    min: 1.0,
    max: 100.0,
    integer: false,
};

const fn period(name: &'static str, default: f64) -> Param {
    Param {
        name,
        default,
        min: 1.0,
        max: 1_000.0,
        integer: true,
    }
}

const fn level(name: &'static str, default: f64) -> Param {
    Param {
        name,
        default,
        min: 1.0,
        max: 99.0,
        integer: false,
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

    /// Parameters; the last is always the position size.
    #[must_use]
    pub const fn params(self) -> &'static [Param] {
        match self {
            Self::EmaCross => const { &[period("Fast", 10.0), period("Slow", 30.0), SIZE] },
            Self::RsiReversion => {
                const {
                    &[
                        period("Length", 14.0),
                        level("Oversold", 30.0),
                        level("Overbought", 70.0),
                        SIZE,
                    ]
                }
            }
            Self::DonchianBreakout => const { &[period("Length", 20.0), SIZE] },
        }
    }
}

/// One strategy on a chart: which one, with what parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct StrategySpec {
    pub kind: BuiltInStrategy,
    /// One value per [`BuiltInStrategy::params`] entry.
    pub params: Vec<f64>,
}

impl StrategySpec {
    #[must_use]
    pub fn new(kind: BuiltInStrategy) -> Self {
        Self {
            kind,
            params: kind.params().iter().map(|p| p.default).collect(),
        }
    }

    /// Builds a spec from saved values, falling back to defaults for missing or invalid ones.
    #[must_use]
    pub fn with_params(kind: BuiltInStrategy, params: &[f64]) -> Self {
        let mut spec = Self::new(kind);
        for (i, value) in params.iter().enumerate().take(spec.params.len()) {
            let _ = spec.set_param(i, *value);
        }
        spec
    }

    /// Sets parameter `index` if `value` is within its limits (whole for periods).
    ///
    /// # Errors
    ///
    /// Returns a message describing the allowed range.
    pub fn set_param(&mut self, index: usize, value: f64) -> Result<(), String> {
        let param = self
            .kind
            .params()
            .get(index)
            .ok_or_else(|| format!("{} has no parameter {index}", self.kind.name()))?;
        let valid = value.is_finite()
            && (param.min..=param.max).contains(&value)
            && (!param.integer || value.fract() == 0.0);
        if !valid {
            return Err(if param.integer {
                format!(
                    "{} must be a whole number from {} to {}",
                    param.name, param.min, param.max
                )
            } else {
                format!("{} must be from {} to {}", param.name, param.min, param.max)
            });
        }
        self.params[index] = value;
        Ok(())
    }

    /// Parameter `index` as a whole number (periods).
    #[must_use]
    pub fn period(&self, index: usize) -> usize {
        self.params.get(index).copied().unwrap_or(1.0).max(1.0) as usize
    }

    /// Share of the starting balance each position uses, from 0.01 to 1.
    #[must_use]
    pub fn size_fraction(&self) -> f64 {
        self.params.last().copied().unwrap_or(SIZE.default) / 100.0
    }

    /// Checks rules that span parameters (fast < slow, oversold < overbought).
    ///
    /// # Errors
    ///
    /// Returns a message saying which rule is broken.
    pub fn validate(&self) -> Result<(), String> {
        match self.kind {
            BuiltInStrategy::EmaCross if self.params[0] >= self.params[1] => Err(format!(
                "Fast ({}) must be shorter than Slow ({})",
                self.params[0], self.params[1]
            )),
            BuiltInStrategy::RsiReversion if self.params[1] >= self.params[2] => Err(format!(
                "Oversold ({}) must be below Overbought ({})",
                self.params[1], self.params[2]
            )),
            _ => Ok(()),
        }
    }
}

impl fmt::Display for StrategySpec {
    /// Legend label without the size, e.g. `EMA Cross 10 30`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.name())?;
        for value in &self.params[..self.params.len().saturating_sub(1)] {
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
            assert_eq!(kind.params().last().unwrap().name, "Size %");
            let spec = StrategySpec::new(kind);
            assert!(spec.validate().is_ok(), "{kind:?} defaults are valid");
            for p in kind.params() {
                assert!((p.min..=p.max).contains(&p.default));
            }
        }
    }

    #[test]
    fn labels_sizes_and_validation() {
        let mut ema = StrategySpec::new(BuiltInStrategy::EmaCross);
        assert_eq!(ema.to_string(), "EMA Cross 10 30");
        assert!((ema.size_fraction() - 0.10).abs() < 1e-12);

        ema.set_param(0, 40.0).unwrap();
        assert!(ema.validate().is_err(), "fast must be shorter than slow");
        assert!(ema.set_param(2, 0.5).is_err(), "size is at least 1%");

        let rsi =
            StrategySpec::with_params(BuiltInStrategy::RsiReversion, &[14.0, 80.0, 20.0, 5.0]);
        assert!(rsi.validate().is_err(), "oversold must be below overbought");
        assert_eq!(rsi.to_string(), "RSI Reversion 14 80 20");
    }
}

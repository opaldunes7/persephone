//! The built-in indicator catalog: what each indicator is called, its parameters, where it is
//! drawn and what it outputs. Computing values is the engine's job; drawing is the chart's.

use std::fmt;

/// Where an indicator is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// On top of the candles, sharing the price scale.
    Overlay,
    /// In its own pane below the price pane, with its own scale.
    Pane,
}

/// How one output series is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStyle {
    Line,
    /// Bars from zero, colored by sign.
    Histogram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    /// Whole numbers only (periods).
    pub integer: bool,
}

impl Param {
    const fn period(name: &'static str, default: f64) -> Self {
        Self {
            name,
            default,
            min: 1.0,
            max: 1_000.0,
            integer: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub name: &'static str,
    pub style: OutputStyle,
}

const fn line(name: &'static str) -> Output {
    Output {
        name,
        style: OutputStyle::Line,
    }
}

/// The indicators that ship with Persephone. Custom ones arrive as plugins (M5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltIn {
    Sma,
    Ema,
    Bollinger,
    Donchian,
    Rsi,
    Macd,
    Stochastic,
    Atr,
}

impl BuiltIn {
    pub const ALL: [Self; 8] = [
        Self::Sma,
        Self::Ema,
        Self::Bollinger,
        Self::Donchian,
        Self::Rsi,
        Self::Macd,
        Self::Stochastic,
        Self::Atr,
    ];

    /// Stable identifier, used when saving layouts.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Sma => "sma",
            Self::Ema => "ema",
            Self::Bollinger => "bollinger",
            Self::Donchian => "donchian",
            Self::Rsi => "rsi",
            Self::Macd => "macd",
            Self::Stochastic => "stochastic",
            Self::Atr => "atr",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.id() == id)
    }

    /// Short name for legends, e.g. "EMA".
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Sma => "SMA",
            Self::Ema => "EMA",
            Self::Bollinger => "BB",
            Self::Donchian => "DC",
            Self::Rsi => "RSI",
            Self::Macd => "MACD",
            Self::Stochastic => "Stoch",
            Self::Atr => "ATR",
        }
    }

    /// Full name for menus.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sma => "Moving Average",
            Self::Ema => "Exponential Moving Average",
            Self::Bollinger => "Bollinger Bands",
            Self::Donchian => "Donchian Channels",
            Self::Rsi => "Relative Strength Index",
            Self::Macd => "MACD",
            Self::Stochastic => "Stochastic",
            Self::Atr => "Average True Range",
        }
    }

    #[must_use]
    pub const fn placement(self) -> Placement {
        match self {
            Self::Sma | Self::Ema | Self::Bollinger | Self::Donchian => Placement::Overlay,
            Self::Rsi | Self::Macd | Self::Stochastic | Self::Atr => Placement::Pane,
        }
    }

    #[must_use]
    pub const fn params(self) -> &'static [Param] {
        const STD_DEV: Param = Param {
            name: "StdDev",
            default: 2.0,
            min: 0.1,
            max: 10.0,
            integer: false,
        };
        match self {
            Self::Sma => const { &[Param::period("Length", 20.0)] },
            Self::Ema => const { &[Param::period("Length", 21.0)] },
            Self::Bollinger => const { &[Param::period("Length", 20.0), STD_DEV] },
            Self::Donchian => const { &[Param::period("Length", 20.0)] },
            Self::Rsi => const { &[Param::period("Length", 14.0)] },
            Self::Macd => {
                const {
                    &[
                        Param::period("Fast", 12.0),
                        Param::period("Slow", 26.0),
                        Param::period("Signal", 9.0),
                    ]
                }
            }
            Self::Stochastic => const { &[Param::period("%K", 14.0), Param::period("%D", 3.0)] },
            Self::Atr => const { &[Param::period("Length", 14.0)] },
        }
    }

    /// Output series, in the order the engine returns them.
    #[must_use]
    pub const fn outputs(self) -> &'static [Output] {
        match self {
            Self::Sma | Self::Ema | Self::Rsi | Self::Atr => const { &[line("Value")] },
            Self::Bollinger | Self::Donchian => {
                const { &[line("Upper"), line("Basis"), line("Lower")] }
            }
            Self::Macd => {
                const {
                    &[
                        Output {
                            name: "Histogram",
                            style: OutputStyle::Histogram,
                        },
                        line("MACD"),
                        line("Signal"),
                    ]
                }
            }
            Self::Stochastic => const { &[line("%K"), line("%D")] },
        }
    }

    /// Outputs whose area between them is shaded (upper, lower).
    #[must_use]
    pub const fn fill(self) -> Option<(usize, usize)> {
        match self {
            Self::Bollinger | Self::Donchian => Some((0, 2)),
            _ => None,
        }
    }

    /// Dashed reference levels in the indicator's pane.
    #[must_use]
    pub const fn levels(self) -> &'static [f64] {
        match self {
            Self::Rsi => &[30.0, 50.0, 70.0],
            Self::Stochastic => &[20.0, 80.0],
            Self::Macd => &[0.0],
            _ => &[],
        }
    }

    /// A fixed scale for oscillators; `None` fits the visible values.
    #[must_use]
    pub const fn range(self) -> Option<(f64, f64)> {
        match self {
            Self::Rsi | Self::Stochastic => Some((0.0, 100.0)),
            _ => None,
        }
    }
}

/// One indicator on a chart: which one, with what parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorSpec {
    pub kind: BuiltIn,
    /// One value per [`BuiltIn::params`] entry.
    pub params: Vec<f64>,
}

impl IndicatorSpec {
    /// The indicator with its default parameters.
    #[must_use]
    pub fn new(kind: BuiltIn) -> Self {
        Self {
            kind,
            params: kind.params().iter().map(|p| p.default).collect(),
        }
    }

    /// Builds a spec from saved values, falling back to defaults for missing or invalid ones.
    #[must_use]
    pub fn with_params(kind: BuiltIn, params: &[f64]) -> Self {
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
            .ok_or_else(|| format!("{} has no parameter {index}", self.kind.short_name()))?;
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
}

impl fmt::Display for IndicatorSpec {
    /// Legend label, e.g. `EMA 21` or `BB 20 2`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.short_name())?;
        for value in &self.params {
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
        for kind in BuiltIn::ALL {
            assert_eq!(BuiltIn::from_id(kind.id()), Some(kind));
            assert!(!kind.outputs().is_empty());
            if let Some((a, b)) = kind.fill() {
                assert!(a < kind.outputs().len() && b < kind.outputs().len());
            }
            for p in kind.params() {
                assert!((p.min..=p.max).contains(&p.default), "{kind:?} {}", p.name);
            }
        }
        assert_eq!(BuiltIn::from_id("nope"), None);
    }

    #[test]
    fn labels_and_defaults() {
        assert_eq!(IndicatorSpec::new(BuiltIn::Ema).to_string(), "EMA 21");
        assert_eq!(
            IndicatorSpec::new(BuiltIn::Bollinger).to_string(),
            "BB 20 2"
        );
        assert_eq!(
            IndicatorSpec::new(BuiltIn::Macd).to_string(),
            "MACD 12 26 9"
        );
    }

    #[test]
    fn params_are_validated() {
        let mut ema = IndicatorSpec::new(BuiltIn::Ema);
        assert!(ema.set_param(0, 50.0).is_ok());
        assert_eq!(ema.period(0), 50);
        assert!(ema.set_param(0, 0.0).is_err());
        assert!(ema.set_param(0, 12.5).is_err(), "periods are whole");
        assert!(ema.set_param(1, 3.0).is_err(), "no such parameter");

        let mut bb = IndicatorSpec::new(BuiltIn::Bollinger);
        assert!(bb.set_param(1, 2.5).is_ok(), "std dev may be fractional");

        let restored = IndicatorSpec::with_params(BuiltIn::Rsi, &[7.0, 99.0]);
        assert_eq!(restored.params, vec![7.0]);
        let fallback = IndicatorSpec::with_params(BuiltIn::Rsi, &[-3.0]);
        assert_eq!(
            fallback.params,
            vec![14.0],
            "invalid saved values fall back"
        );
    }
}

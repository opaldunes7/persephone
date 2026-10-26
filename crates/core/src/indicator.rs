//! Indicators: the built-in catalog, and the description every indicator (built-in or
//! plugin) shares: its name, parameters, where it is drawn and what it outputs. Computing
//! values is the engine's (or the plugin host's) job; drawing is the chart's.

use std::{borrow::Cow, fmt, sync::Arc};

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

/// A numeric parameter the user can edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: Cow<'static, str>,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    /// Whole numbers only (periods).
    pub integer: bool,
}

impl Param {
    /// A look-back period from 1 to 1000 bars.
    #[must_use]
    pub const fn period(name: &'static str, default: f64) -> Self {
        Self {
            name: Cow::Borrowed(name),
            default,
            min: 1.0,
            max: 1_000.0,
            integer: true,
        }
    }

    /// Whether `value` is allowed.
    ///
    /// # Errors
    ///
    /// Returns a message describing the allowed range.
    pub fn check(&self, value: f64) -> Result<(), String> {
        let valid = value.is_finite()
            && (self.min..=self.max).contains(&value)
            && (!self.integer || value.fract() == 0.0);
        if valid {
            Ok(())
        } else if self.integer {
            Err(format!(
                "{} must be a whole number from {} to {}",
                self.name, self.min, self.max
            ))
        } else {
            Err(format!(
                "{} must be from {} to {}",
                self.name, self.min, self.max
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub name: Cow<'static, str>,
    pub style: OutputStyle,
}

const fn line(name: &'static str) -> Output {
    Output {
        name: Cow::Borrowed(name),
        style: OutputStyle::Line,
    }
}

/// Everything about an indicator except its values.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorInfo {
    /// Full name for menus, e.g. "Exponential Moving Average".
    pub name: String,
    /// Legend name, e.g. "EMA".
    pub short_name: String,
    pub placement: Placement,
    pub params: Vec<Param>,
    /// Output series, in the order values are computed.
    pub outputs: Vec<Output>,
    /// Outputs whose area between them is shaded (upper, lower).
    pub fill: Option<(usize, usize)>,
    /// Dashed reference levels in the indicator's pane.
    pub levels: Vec<f64>,
    /// A fixed scale for its pane; `None` fits the visible values.
    pub range: Option<(f64, f64)>,
}

/// The indicators that ship with Persephone. Custom ones arrive as plugins.
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
            name: Cow::Borrowed("StdDev"),
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
        const HISTOGRAM: Output = Output {
            name: Cow::Borrowed("Histogram"),
            style: OutputStyle::Histogram,
        };
        match self {
            Self::Sma | Self::Ema | Self::Rsi | Self::Atr => const { &[line("Value")] },
            Self::Bollinger | Self::Donchian => {
                const { &[line("Upper"), line("Basis"), line("Lower")] }
            }
            Self::Macd => const { &[HISTOGRAM, line("MACD"), line("Signal")] },
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

    #[must_use]
    pub fn info(self) -> IndicatorInfo {
        IndicatorInfo {
            name: self.name().into(),
            short_name: self.short_name().into(),
            placement: self.placement(),
            params: self.params().to_vec(),
            outputs: self.outputs().to_vec(),
            fill: self.fill(),
            levels: self.levels().to_vec(),
            range: self.range(),
        }
    }
}

/// Which indicator: a built-in, or a plugin by id (its file name without `.wasm`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IndicatorKind {
    BuiltIn(BuiltIn),
    Plugin(String),
}

/// One indicator on a chart: which one, its description, and its parameter values.
#[derive(Clone, Debug)]
pub struct IndicatorSpec {
    pub kind: IndicatorKind,
    pub info: Arc<IndicatorInfo>,
    /// One value per `info.params` entry.
    pub params: Vec<f64>,
}

impl PartialEq for IndicatorSpec {
    /// Same indicator with the same values; the description follows from the kind.
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.params == other.params
    }
}

impl IndicatorSpec {
    /// A built-in with its default parameters.
    #[must_use]
    pub fn new(kind: BuiltIn) -> Self {
        Self::from_info(IndicatorKind::BuiltIn(kind), Arc::new(kind.info()), &[])
    }

    /// A built-in with saved values, falling back to defaults for missing or invalid ones.
    #[must_use]
    pub fn with_params(kind: BuiltIn, params: &[f64]) -> Self {
        Self::from_info(IndicatorKind::BuiltIn(kind), Arc::new(kind.info()), params)
    }

    /// Any indicator from its description, with saved values where they are still valid.
    #[must_use]
    pub fn from_info(kind: IndicatorKind, info: Arc<IndicatorInfo>, saved: &[f64]) -> Self {
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
            .ok_or_else(|| format!("{} has no parameter {index}", self.info.short_name))?;
        param.check(value)?;
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
        f.write_str(&self.info.short_name)?;
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
            let info = kind.info();
            assert!(!info.outputs.is_empty());
            if let Some((a, b)) = info.fill {
                assert!(a < info.outputs.len() && b < info.outputs.len());
            }
            for p in &info.params {
                assert!(p.check(p.default).is_ok(), "{kind:?} {}", p.name);
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

    #[test]
    fn plugin_specs_describe_themselves() {
        let info = Arc::new(IndicatorInfo {
            name: "Hull Moving Average".into(),
            short_name: "HMA".into(),
            placement: Placement::Overlay,
            params: vec![Param::period("Length", 9.0)],
            outputs: vec![line("HMA")],
            fill: None,
            levels: Vec::new(),
            range: None,
        });
        let mut hma = IndicatorSpec::from_info(IndicatorKind::Plugin("hma".into()), info, &[16.0]);
        assert_eq!(hma.to_string(), "HMA 16");
        assert!(hma.set_param(0, 2_000.0).is_err());
        assert_ne!(hma, IndicatorSpec::new(BuiltIn::Sma));
    }
}

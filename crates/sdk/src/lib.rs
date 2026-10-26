//! Write Persephone plugins in Rust.
//!
//! A plugin is a small crate built for `wasm32-wasip2` that implements [`Indicator`] or
//! [`Strategy`] and exports it with [`export_indicator!`] or [`export_strategy!`]. Turn on the
//! matching crate feature (`indicator` or `strategy`), build with
//! `cargo build --release --target wasm32-wasip2`, and drop the `.wasm` file into
//! Persephone's plugins folder; it loads (and reloads) on its own.
//!
//! ```ignore
//! use persephone_sdk::{Bar, Indicator, IndicatorInfo, Param, export_indicator, ta};
//!
//! struct Sma;
//!
//! impl Indicator for Sma {
//!     fn describe() -> IndicatorInfo {
//!         IndicatorInfo::overlay("Simple Moving Average", "SMA")
//!             .param(Param::period("Length", 20))
//!             .line("SMA")
//!     }
//!
//!     fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
//!         let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
//!         Ok(vec![ta::sma(&closes, params[0] as usize)])
//!     }
//! }
//!
//! export_indicator!(Sma);
//! ```

// Lets the generated bindings name this crate the same way from inside it and from plugins.
extern crate self as persephone_sdk;

pub mod ta;

#[doc(hidden)]
pub use wit_bindgen as __wit_bindgen;

/// One closed OHLCV bar. `ts` is the bar's open time in nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bar {
    pub ts: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// A numeric parameter the user can edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub integer: bool,
}

impl Param {
    /// A look-back period from 1 to 1000 bars.
    #[must_use]
    pub fn period(name: &str, default: u32) -> Self {
        Self {
            name: name.into(),
            default: f64::from(default),
            min: 1.0,
            max: 1_000.0,
            integer: true,
        }
    }

    /// Any number within `[min, max]`.
    #[must_use]
    pub fn number(name: &str, default: f64, min: f64, max: f64) -> Self {
        Self {
            name: name.into(),
            default,
            min,
            max,
            integer: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// On the price pane, sharing its scale.
    Overlay,
    /// In a pane of its own below the prices.
    Pane,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStyle {
    Line,
    /// Bars from zero, colored by sign.
    Histogram,
}

/// What an indicator is called, its parameters, where it's drawn and what it outputs.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorInfo {
    pub name: String,
    pub short_name: String,
    pub placement: Placement,
    pub params: Vec<Param>,
    pub outputs: Vec<(String, OutputStyle)>,
    pub fill: Option<(u32, u32)>,
    pub levels: Vec<f64>,
    pub range: Option<(f64, f64)>,
}

impl IndicatorInfo {
    /// An indicator drawn over the prices, e.g. a moving average.
    #[must_use]
    pub fn overlay(name: &str, short_name: &str) -> Self {
        Self::new(name, short_name, Placement::Overlay)
    }

    /// An indicator in its own pane, e.g. an oscillator.
    #[must_use]
    pub fn pane(name: &str, short_name: &str) -> Self {
        Self::new(name, short_name, Placement::Pane)
    }

    fn new(name: &str, short_name: &str, placement: Placement) -> Self {
        Self {
            name: name.into(),
            short_name: short_name.into(),
            placement,
            params: Vec::new(),
            outputs: Vec::new(),
            fill: None,
            levels: Vec::new(),
            range: None,
        }
    }

    #[must_use]
    pub fn param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// Adds an output drawn as a line.
    #[must_use]
    pub fn line(mut self, name: &str) -> Self {
        self.outputs.push((name.into(), OutputStyle::Line));
        self
    }

    /// Adds an output drawn as bars from zero.
    #[must_use]
    pub fn histogram(mut self, name: &str) -> Self {
        self.outputs.push((name.into(), OutputStyle::Histogram));
        self
    }

    /// Shades the area between outputs `upper` and `lower`.
    #[must_use]
    pub fn fill(mut self, upper: u32, lower: u32) -> Self {
        self.fill = Some((upper, lower));
        self
    }

    /// A dashed reference level in the indicator's pane.
    #[must_use]
    pub fn level(mut self, value: f64) -> Self {
        self.levels.push(value);
        self
    }

    /// A fixed scale for the indicator's pane, e.g. 0 to 100.
    #[must_use]
    pub fn range(mut self, low: f64, high: f64) -> Self {
        self.range = Some((low, high));
        self
    }
}

/// What a strategy is called and its parameters. Persephone adds a "Size %" parameter of its
/// own, so don't declare one.
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyInfo {
    pub name: String,
    pub description: String,
    pub params: Vec<Param>,
}

impl StrategyInfo {
    #[must_use]
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
        }
    }

    #[must_use]
    pub fn param(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }
}

/// An indicator: values for every bar, all at once.
pub trait Indicator {
    fn describe() -> IndicatorInfo;

    /// One series per output, each as long as `bars`; use `f64::NAN` where there is no value
    /// yet. `params` follow the order of [`IndicatorInfo::params`].
    ///
    /// # Errors
    ///
    /// Return a message to show in the chart's legend instead of values.
    fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String>;
}

/// A strategy: sees each closed bar in order and says which position it wants. Persephone
/// places the orders and runs the backtest.
pub trait Strategy: Sized + 'static {
    fn describe() -> StrategyInfo;

    /// Called before the first bar with the user's parameters, in the order of
    /// [`StrategyInfo::params`].
    ///
    /// # Errors
    ///
    /// Return a message (e.g. "Fast must be shorter than Slow") to stop the backtest.
    fn start(params: &[f64]) -> Result<Self, String>;

    /// `Some(1)` for long, `Some(-1)` for short, `Some(0)` for flat, `None` to keep the
    /// current position.
    fn on_bar(&mut self, bar: &Bar) -> Option<i8>;
}

#[cfg(feature = "indicator")]
#[doc(hidden)]
pub mod indicator_bindings {
    wit_bindgen::generate!({
        world: "indicator",
        path: "../../wit",
        pub_export_macro: true,
        export_macro_name: "export_indicator_world",
        default_bindings_module: "persephone_sdk::indicator_bindings",
        runtime_path: "persephone_sdk::__wit_bindgen::rt",
    });

    use persephone::plugin::types as wit;

    impl From<wit::Bar> for crate::Bar {
        fn from(b: wit::Bar) -> Self {
            Self {
                ts: b.ts,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: b.volume,
            }
        }
    }

    impl From<crate::IndicatorInfo> for wit::IndicatorInfo {
        fn from(info: crate::IndicatorInfo) -> Self {
            Self {
                name: info.name,
                short_name: info.short_name,
                placement: match info.placement {
                    crate::Placement::Overlay => wit::Placement::Overlay,
                    crate::Placement::Pane => wit::Placement::Pane,
                },
                params: info.params.into_iter().map(param).collect(),
                outputs: info
                    .outputs
                    .into_iter()
                    .map(|(name, style)| wit::Output {
                        name,
                        style: match style {
                            crate::OutputStyle::Line => wit::OutputStyle::Line,
                            crate::OutputStyle::Histogram => wit::OutputStyle::Histogram,
                        },
                    })
                    .collect(),
                fill: info.fill,
                levels: info.levels,
                range: info.range,
            }
        }
    }

    fn param(p: crate::Param) -> wit::Param {
        wit::Param {
            name: p.name,
            default: p.default,
            min: p.min,
            max: p.max,
            integer: p.integer,
        }
    }
}

#[cfg(feature = "strategy")]
#[doc(hidden)]
pub mod strategy_bindings {
    wit_bindgen::generate!({
        world: "strategy",
        path: "../../wit",
        pub_export_macro: true,
        export_macro_name: "export_strategy_world",
        default_bindings_module: "persephone_sdk::strategy_bindings",
        runtime_path: "persephone_sdk::__wit_bindgen::rt",
    });

    use persephone::plugin::types as wit;

    impl From<wit::Bar> for crate::Bar {
        fn from(b: wit::Bar) -> Self {
            Self {
                ts: b.ts,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: b.volume,
            }
        }
    }

    impl From<crate::StrategyInfo> for wit::StrategyInfo {
        fn from(info: crate::StrategyInfo) -> Self {
            Self {
                name: info.name,
                description: info.description,
                params: info
                    .params
                    .into_iter()
                    .map(|p| wit::Param {
                        name: p.name,
                        default: p.default,
                        min: p.min,
                        max: p.max,
                        integer: p.integer,
                    })
                    .collect(),
            }
        }
    }
}

/// Exports a type implementing [`Indicator`] as the plugin's indicator. Needs the
/// `indicator` feature.
#[cfg(feature = "indicator")]
#[macro_export]
macro_rules! export_indicator {
    ($indicator:ty) => {
        const _: () = {
            struct __PersephonePlugin;

            impl $crate::indicator_bindings::Guest for __PersephonePlugin {
                fn describe() -> $crate::indicator_bindings::persephone::plugin::types::IndicatorInfo {
                    <$indicator as $crate::Indicator>::describe().into()
                }

                fn compute(
                    params: ::std::vec::Vec<f64>,
                    bars: ::std::vec::Vec<$crate::indicator_bindings::persephone::plugin::types::Bar>,
                ) -> ::std::result::Result<::std::vec::Vec<::std::vec::Vec<f64>>, ::std::string::String> {
                    let bars: ::std::vec::Vec<$crate::Bar> = bars.into_iter().map(::core::convert::Into::into).collect();
                    <$indicator as $crate::Indicator>::compute(&params, &bars)
                }
            }

            $crate::indicator_bindings::export_indicator_world!(__PersephonePlugin with_types_in $crate::indicator_bindings);
        };
    };
}

/// Exports a type implementing [`Strategy`] as the plugin's strategy. Needs the `strategy`
/// feature.
#[cfg(feature = "strategy")]
#[macro_export]
macro_rules! export_strategy {
    ($strategy:ty) => {
        const _: () = {
            ::std::thread_local! {
                static STATE: ::core::cell::RefCell<::core::option::Option<$strategy>> =
                    const { ::core::cell::RefCell::new(::core::option::Option::None) };
            }

            struct __PersephonePlugin;

            impl $crate::strategy_bindings::Guest for __PersephonePlugin {
                fn describe() -> $crate::strategy_bindings::persephone::plugin::types::StrategyInfo {
                    <$strategy as $crate::Strategy>::describe().into()
                }

                fn start(params: ::std::vec::Vec<f64>) -> ::std::result::Result<(), ::std::string::String> {
                    let strategy = <$strategy as $crate::Strategy>::start(&params)?;
                    STATE.with(|state| *state.borrow_mut() = ::core::option::Option::Some(strategy));
                    ::std::result::Result::Ok(())
                }

                fn on_bar(bar: $crate::strategy_bindings::persephone::plugin::types::Bar) -> ::core::option::Option<i8> {
                    let bar: $crate::Bar = bar.into();
                    STATE.with(|state| {
                        state
                            .borrow_mut()
                            .as_mut()
                            .and_then(|strategy| <$strategy as $crate::Strategy>::on_bar(strategy, &bar))
                    })
                }
            }

            $crate::strategy_bindings::export_strategy_world!(__PersephonePlugin with_types_in $crate::strategy_bindings);
        };
    };
}

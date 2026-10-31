//! What the Persephone app and Lab share, with no interface of its own: a chart's backtest
//! settings, loading markets, and the tests beyond a backtest.

pub mod calendar;
pub mod logic;
pub mod market;
pub mod optimize;
pub mod results;
pub mod robustness;
pub mod settings;

use self::settings::TestKind;

/// A whole number with commas: `1,234,567`.
#[must_use]
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Bars loaded before a span's start for the strategy to warm up on, without trading.
pub const WARM_UP_BARS: u64 = 500;

/// What the table of runs can be sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunColumn {
    #[default]
    Return,
    Fell,
    Run,
}

/// A box in the settings section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Capital,
    Commission,
    Slippage,
    Spread,
    Latency,
    Leverage,
    SessionFrom,
    SessionTo,
    SessionZone,
    Runs,
    Ruin,
    Blocks,
    /// An Optimize variable's range: the first variable or the second.
    Min(usize),
    Max(usize),
    Step(usize),
}

impl Field {
    /// Whether it's one of `test`'s own settings, which that test applies when it's run.
    #[must_use]
    pub const fn belongs_to(self, test: TestKind) -> bool {
        match test {
            TestKind::Backtest => false,
            TestKind::MonteCarlo => matches!(self, Self::Runs | Self::Ruin),
            TestKind::RandomBlocks => matches!(self, Self::Blocks),
            TestKind::Optimize => self.belongs_to_tests(),
        }
    }

    /// Whether it's one of the tests' own settings, which don't change the backtest itself.
    #[must_use]
    pub const fn belongs_to_tests(self) -> bool {
        matches!(
            self,
            Self::Runs | Self::Ruin | Self::Blocks | Self::Min(_) | Self::Max(_) | Self::Step(_)
        )
    }
}

//! What a backtest assumes about fills and costs ([`SimSettings`]), and the rules a strategy
//! trades by ([`TradingRules`]), in backtests and live alike.

use std::{fmt, sync::Arc};

use persephone_core::UnixNanos;

/// How a backtest fills orders and what they cost beyond the fee rates. The default assumes
/// nothing: no flat fee, slippage, spread or latency, and no leverage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimSettings {
    /// A flat commission per order, in the quote currency, instead of the fee rates.
    pub fee_per_order: Option<f64>,
    /// Ticks every market order fills worse than the price it met.
    pub slippage_ticks: u32,
    /// A spread to assume when the data has none (bars and trades), in ticks: buys pay the
    /// larger half of it above the price and sells the rest below. Quotes bring their own.
    pub spread_ticks: u32,
    /// How long orders take to reach the venue.
    pub latency_ms: u32,
    /// How many times its share of the capital a position is worth (1 is no leverage).
    pub leverage: f64,
}

impl Default for SimSettings {
    fn default() -> Self {
        Self {
            fee_per_order: None,
            slippage_ticks: 0,
            spread_ticks: 0,
            latency_ms: 0,
            leverage: 1.0,
        }
    }
}

/// Rules on what a strategy may do, whatever its signal says.
#[derive(Clone, Debug, Default)]
pub struct TradingRules {
    /// Do the opposite of what the strategy says: long for short, short for long. Going flat is
    /// still going flat.
    pub invert: bool,
    pub direction: Direction,
    /// When positions may be opened; any time without one.
    pub session: Option<Session>,
}

/// Which way a strategy may hold positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Both,
    /// Short signals close longs instead.
    LongOnly,
    /// Long signals close shorts instead.
    ShortOnly,
}

impl Direction {
    /// `target` (1 long, -1 short, 0 flat) as this direction allows it.
    #[must_use]
    pub fn allow(self, target: i8) -> i8 {
        match self {
            Self::Both => target,
            Self::LongOnly => target.max(0),
            Self::ShortOnly => target.min(0),
        }
    }
}

/// When a strategy may open positions. Outside it, signals can still close them.
#[derive(Clone)]
pub struct Session {
    /// Whether a moment (a bar's close) is inside the session.
    pub contains: Arc<dyn Fn(UnixNanos) -> bool + Send + Sync>,
    /// Close any position once the session is over.
    pub close_after: bool,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("close_after", &self.close_after)
            .finish_non_exhaustive()
    }
}

impl TradingRules {
    /// The position to take at `ts`, given what the strategy wants (`None` keeps what it has)
    /// and what it holds (1, -1 or 0); `None` changes nothing.
    #[must_use]
    pub fn target(&self, wanted: Option<i8>, held: i8, ts: UnixNanos) -> Option<i8> {
        let wanted = wanted
            .map(|target| if self.invert { -target } else { target })
            .map(|target| self.direction.allow(target));
        let Some(session) = self.session.as_ref().filter(|s| !(s.contains)(ts)) else {
            return wanted;
        };
        if session.close_after && held != 0 {
            return Some(0);
        }
        // Outside the session a signal may close a position, but not open or reverse one.
        match wanted {
            Some(target) if target == held => None,
            Some(_) if held != 0 => Some(0),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(
        direction: Direction,
        open_until: Option<UnixNanos>,
        close_after: bool,
    ) -> TradingRules {
        TradingRules {
            invert: false,
            direction,
            session: open_until.map(|until| Session {
                contains: Arc::new(move |ts| ts < until),
                close_after,
            }),
        }
    }

    #[test]
    fn directions_turn_the_other_side_into_flat() {
        let long_only = rules(Direction::LongOnly, None, false);
        assert_eq!(long_only.target(Some(-1), 1, 0), Some(0));
        assert_eq!(long_only.target(Some(1), 0, 0), Some(1));
        let short_only = rules(Direction::ShortOnly, None, false);
        assert_eq!(short_only.target(Some(1), -1, 0), Some(0));
        assert_eq!(rules(Direction::Both, None, false).target(None, 1, 0), None);
    }

    #[test]
    fn inverting_swaps_long_and_short_but_flat_stays_flat() {
        let mut inverted = rules(Direction::Both, None, false);
        inverted.invert = true;
        assert_eq!(inverted.target(Some(1), 0, 0), Some(-1));
        assert_eq!(inverted.target(Some(-1), 0, 0), Some(1));
        assert_eq!(inverted.target(Some(0), 1, 0), Some(0));
        assert_eq!(
            inverted.target(None, 1, 0),
            None,
            "no signal keeps what it has"
        );
    }

    #[test]
    fn inverting_comes_before_the_other_rules() {
        // Long only still means only long positions: what were short signals now open them.
        let mut long_only = rules(Direction::LongOnly, None, false);
        long_only.invert = true;
        assert_eq!(long_only.target(Some(-1), 0, 0), Some(1));
        assert_eq!(long_only.target(Some(1), 1, 0), Some(0));

        // Outside a session, it can close but not open, the inverted way round too.
        let mut session = rules(Direction::Both, Some(100), false);
        session.invert = true;
        assert_eq!(session.target(Some(-1), 0, 150), None, "no entry");
        assert_eq!(
            session.target(Some(-1), 1, 150),
            None,
            "already where it's told to be"
        );
        assert_eq!(
            session.target(Some(1), 1, 150),
            Some(0),
            "a reversal only closes"
        );
    }

    #[test]
    fn outside_the_session_positions_close_but_dont_open() {
        let session = rules(Direction::Both, Some(100), false);
        assert_eq!(session.target(Some(1), 0, 50), Some(1), "inside");
        assert_eq!(session.target(Some(1), 0, 150), None, "no entry");
        assert_eq!(
            session.target(Some(-1), 1, 150),
            Some(0),
            "a reversal only closes"
        );
        assert_eq!(session.target(Some(0), -1, 150), Some(0));
        assert_eq!(session.target(None, 1, 150), None, "kept");

        let closing = rules(Direction::Both, Some(100), true);
        assert_eq!(closing.target(None, 1, 150), Some(0), "closed at the end");
        assert_eq!(closing.target(None, 0, 150), None);
    }
}

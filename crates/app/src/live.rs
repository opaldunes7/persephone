//! Trading a chart's strategy on bars as they close: on paper, or on Binance's testnet when
//! its keys are set. Real money is only in `persephone-cli live`.

use persephone_core::{Candle, UnixNanos};
use persephone_engine::live::{Book, LiveEvent, LiveFill, LiveMode, LiveSession};

/// A chart's live session and what it has done. It stays after the session ends, to show how
/// it went.
#[derive(Debug)]
pub struct Live {
    pub mode: LiveMode,
    /// Tells this session's events from an earlier one's.
    pub id: u64,
    pub state: LiveState,
    /// Set while the session runs.
    pub(crate) session: Option<LiveSession>,
    pub book: Book,
    pub fills: Vec<LiveFill>,
    /// Open time of the first bar the session saw.
    pub since: Option<UnixNanos>,
    /// The latest refused order, or other trouble.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// Fetching the latest bars to warm the strategy up.
    Preparing,
    Connecting,
    /// Connected and trading each bar as it closes.
    Running,
    Stopping,
    /// Over, with the reason if it failed.
    Stopped(Option<String>),
}

impl Live {
    #[must_use]
    pub fn new(mode: LiveMode, id: u64) -> Self {
        Self {
            mode,
            id,
            state: LiveState::Preparing,
            session: None,
            book: Book::default(),
            fills: Vec::new(),
            since: None,
            problem: None,
        }
    }

    /// Takes in an event; returns the bar it brought, if any.
    pub fn apply(&mut self, event: LiveEvent) -> Option<Candle> {
        self.book.apply(&event);
        match event {
            LiveEvent::Started => {
                if self.state == LiveState::Connecting {
                    self.state = LiveState::Running;
                }
            }
            LiveEvent::Bar(bar) => {
                self.since.get_or_insert(bar.ts);
                return Some(bar);
            }
            LiveEvent::Fill(fill) => self.fills.push(fill),
            LiveEvent::Problem(message) => self.problem = Some(message),
            LiveEvent::Stopped(outcome) => {
                self.state = LiveState::Stopped(outcome);
                self.session = None;
            }
        }
        None
    }

    /// Asks the session to stop (closing its position); it says when it has.
    pub fn stop(&mut self) {
        match &self.session {
            Some(session) => {
                session.stop();
                self.state = LiveState::Stopping;
            }
            None => self.state = LiveState::Stopped(None),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.state, LiveState::Stopped(_))
    }

    /// What it's doing, in a few words: "paper · connecting…".
    #[must_use]
    pub fn status(&self) -> String {
        let doing = match &self.state {
            LiveState::Preparing => "getting the latest bars…",
            LiveState::Connecting => "connecting…",
            LiveState::Running if self.since.is_none() => "waiting for a bar to close",
            LiveState::Running => "trading",
            LiveState::Stopping => "closing out…",
            LiveState::Stopped(None) => "stopped",
            LiveState::Stopped(Some(_)) => "failed",
        };
        format!("{} · {doing}", self.mode.label())
    }
}

/// Adds a bar that just closed to `candles` (oldest first): it replaces the forming bar with the
/// same open time, or comes after the last. Returns whether anything changed.
pub fn merge(candles: &mut Vec<Candle>, bar: Candle) -> bool {
    match candles.last_mut() {
        Some(last) if last.ts == bar.ts => {
            *last = bar;
            true
        }
        Some(last) if last.ts > bar.ts => false,
        _ => {
            candles.push(bar);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use persephone_core::Side;

    use super::*;

    fn bar(ts: UnixNanos, close: f64) -> Candle {
        Candle {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    #[test]
    fn closed_bars_replace_the_forming_one_or_follow() {
        let mut candles = vec![bar(0, 1.0), bar(60, 2.0)];
        assert!(
            merge(&mut candles, bar(60, 2.5)),
            "the forming bar, now closed"
        );
        assert!(merge(&mut candles, bar(120, 3.0)));
        assert!(!merge(&mut candles, bar(0, 9.0)), "too old");
        let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
        assert_eq!(closes, [1.0, 2.5, 3.0]);
    }

    #[test]
    fn a_session_tracks_its_state_and_trades() {
        let mut live = Live::new(LiveMode::Paper, 1);
        assert_eq!(live.status(), "paper · getting the latest bars…");
        live.state = LiveState::Connecting;
        live.apply(LiveEvent::Started);
        assert_eq!(live.status(), "paper · waiting for a bar to close");
        assert_eq!(
            live.apply(LiveEvent::Bar(bar(60, 100.0))),
            Some(bar(60, 100.0))
        );
        live.apply(LiveEvent::Fill(LiveFill {
            ts: 61,
            bar: 60,
            side: Side::Buy,
            price: 100.0,
            qty: 1.0,
            fee: 0.0,
        }));
        live.apply(LiveEvent::Bar(bar(120, 104.0)));
        assert_eq!((live.since, live.fills.len()), (Some(60), 1));
        assert!((live.book.pnl() - 4.0).abs() < 1e-9);
        live.apply(LiveEvent::Stopped(Some("gone".into())));
        assert!(!live.is_active());
        assert_eq!(live.status(), "paper · failed");
    }
}

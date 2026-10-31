//! The one thing a strategy has to do: look at each closed bar and say which position it
//! wants. Built-ins implement [`Signal`] in the engine; plugins implement it in the plugin
//! host, which hands the engine a [`SignalFactory`].

use std::fmt;

use crate::Candle;

/// A trading rule, fed closed bars in order.
pub trait Signal {
    /// Returns the position it wants, `1` long, `-1` short or `0` flat, or `None` to keep
    /// whatever it has.
    ///
    /// # Errors
    ///
    /// Returns a message if the rule failed (e.g. a plugin crashed); the backtest stops.
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String>;

    /// What's really held, told before each bar: `1` long, `-1` short or `0` flat, and what it
    /// entered at. A rule that cares needs this, as what it asks for isn't always what happens:
    /// signals are dropped while warming up, and rules may invert or refuse them.
    fn holding(&mut self, _position: i8, _entry_price: f64) {}

    /// What the report should say about this run once it's over (a plugin's AI use, say).
    fn notes(&mut self) -> Vec<Note> {
        Vec::new()
    }
}

/// A line for the backtest report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    /// Something that makes the results less trustworthy.
    pub warning: bool,
}

/// Makes a fresh [`Signal`] for each backtest, on the backtest's own thread.
pub trait SignalFactory: Send + Sync + fmt::Debug {
    /// `params` are the strategy's own parameters (without the position size).
    ///
    /// # Errors
    ///
    /// Returns a message if the strategy can't start with these parameters.
    fn create(&self, params: &[f64]) -> Result<Box<dyn Signal>, String>;
}

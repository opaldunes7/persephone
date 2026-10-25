//! Strategies the engine runs.
//!
//! A strategy here is a [`Signal`]: it sees each closed bar and says which position it wants.
//! [`SignalStrategy`] is the one Nautilus strategy that turns those wishes into orders, so
//! adding a strategy never touches Nautilus. WASM plugin strategies (M5) plug in the same way.

pub(crate) mod builtins;
pub(crate) mod signal;

pub(crate) use signal::{Signal, SignalStrategy};

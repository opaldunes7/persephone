//! Strategies the engine runs.
//!
//! A strategy here is a [`Signal`]: it sees each closed bar and says which position it wants.
//! [`SignalStrategy`] is the one Nautilus strategy that turns those wishes into orders, so
//! adding a strategy never touches Nautilus. Plugin strategies arrive through a
//! [`SignalFactory`] and run the same way.
//!
//! [`Signal`]: persephone_core::signal::Signal
//! [`SignalFactory`]: persephone_core::signal::SignalFactory

pub(crate) mod builtins;
pub(crate) mod signal;

pub(crate) use signal::SignalStrategy;
#[cfg(feature = "live")]
pub(crate) use signal::Watcher;

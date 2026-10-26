//! Reads the clock and the system's randomness: the host must give the same answers every
//! run, so backtests repeat exactly.

use std::{
    collections::hash_map::RandomState,
    hash::BuildHasher,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use persephone_sdk::{Bar, Indicator, IndicatorInfo, export_indicator};

struct Clock;

impl Indicator for Clock {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::pane("Clock", "CLOCK")
            .line("Now")
            .line("Elapsed")
            .line("Random")
    }

    fn compute(_params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let started = Instant::now();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        let random = RandomState::new().hash_one(42_u32);
        let elapsed = started.elapsed();
        Ok(vec![
            vec![now.as_secs_f64(); bars.len()],
            vec![elapsed.as_secs_f64(); bars.len()],
            vec![random as f64; bars.len()],
        ])
    }
}

export_indicator!(Clock);

//! Panics while computing: the host must report it, not crash.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, export_indicator};

struct Panics;

impl Indicator for Panics {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::overlay("Panics", "PANIC").line("Never")
    }

    fn compute(_params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        panic!("boom after {} bars", bars.len());
    }
}

export_indicator!(Panics);

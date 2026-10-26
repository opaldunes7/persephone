//! Never finishes computing: the host must stop it.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, export_indicator};

struct Spins;

impl Indicator for Spins {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::overlay("Spins", "SPIN").line("Never")
    }

    fn compute(_params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let mut x = bars.len() as u64;
        loop {
            x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
        }
    }
}

export_indicator!(Spins);

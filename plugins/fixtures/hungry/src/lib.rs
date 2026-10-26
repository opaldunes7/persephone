//! Tries to take far more memory than plugins get: the host must refuse.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, export_indicator};

struct Hungry;

impl Indicator for Hungry {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::overlay("Hungry", "HUNGRY").line("Never")
    }

    fn compute(_params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let mut blocks = Vec::new();
        loop {
            blocks.push(std::hint::black_box(vec![bars.len() as u8; 64 << 20]));
        }
    }
}

export_indicator!(Hungry);

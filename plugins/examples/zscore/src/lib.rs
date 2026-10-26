//! Z-score: how many standard deviations the close is from its moving average. Drawn in a pane
//! of its own with dashed levels at ±2.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, Param, export_indicator, ta};

struct ZScore;

impl Indicator for ZScore {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::pane("Z-Score", "Z")
            .param(Param::period("Length", 50))
            .line("Z")
            .level(-2.0)
            .level(0.0)
            .level(2.0)
    }

    fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let length = params[0] as usize;
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let mean = ta::sma(&closes, length);
        let z = (0..closes.len())
            .map(|i| {
                if i + 1 < length {
                    return f64::NAN;
                }
                let window = &closes[i + 1 - length..=i];
                let variance =
                    window.iter().map(|c| (c - mean[i]).powi(2)).sum::<f64>() / length as f64;
                let deviation = variance.sqrt();
                if deviation == 0.0 {
                    0.0
                } else {
                    (closes[i] - mean[i]) / deviation
                }
            })
            .collect();
        Ok(vec![z])
    }
}

export_indicator!(ZScore);

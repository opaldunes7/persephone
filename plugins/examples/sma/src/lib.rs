//! Simple moving average: the smallest useful indicator plugin. It matches Persephone's
//! built-in SMA.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, Param, export_indicator, ta};

struct Sma;

impl Indicator for Sma {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::overlay("SMA (plugin)", "pSMA")
            .param(Param::period("Length", 20))
            .line("SMA")
    }

    fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        Ok(vec![ta::sma(&closes, params[0] as usize)])
    }
}

export_indicator!(Sma);

//! Runs the `probe` model over every bar at once, for the host's ML tests: one feature per
//! bar (the close over 10,000), one output per bar.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, export_indicator, ml};

struct MlProbe;

impl Indicator for MlProbe {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::pane("ML probe", "MLP").line("Model")
    }

    fn compute(_params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let rows: Vec<Vec<f32>> = bars
            .iter()
            .map(|b| vec![(b.close / 10_000.0) as f32])
            .collect();
        let outputs = ml::run("probe", &ml::Tensor::rows(&rows))?;
        Ok(vec![
            outputs[0].data.iter().map(|&v| f64::from(v)).collect(),
        ])
    }
}

export_indicator!(MlProbe);

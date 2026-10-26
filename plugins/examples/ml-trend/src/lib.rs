//! Trades an ONNX model's view of the next bar: the model gets the last four bar returns (in
//! percent) and answers the probability that the next bar closes higher. Above the threshold
//! it goes long, below one minus the threshold short; in between it keeps its position.
//!
//! It runs `models/trend.onnx`: `persephone-cli models example` writes a starter model there,
//! and `train.py` (next to this file) trains a real one on your bars.

use std::collections::VecDeque;

use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, export_strategy, ml};

const MODEL: &str = "trend";
const FEATURES: usize = 4;

struct MlTrend {
    threshold: f32,
    closes: VecDeque<f64>,
}

impl Strategy for MlTrend {
    fn describe() -> StrategyInfo {
        StrategyInfo::new(
            "ML Trend (plugin)",
            "Trades an ONNX model's call on the next bar",
        )
        .param(Param::number("Threshold %", 55.0, 50.5, 95.0))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        Ok(Self {
            threshold: (params[0] / 100.0) as f32,
            closes: VecDeque::with_capacity(FEATURES + 1),
        })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        self.closes.push_back(bar.close);
        if self.closes.len() > FEATURES + 1 {
            self.closes.pop_front();
        }
        if self.closes.len() <= FEATURES {
            return None;
        }
        let returns: Vec<f32> = self
            .closes
            .iter()
            .zip(self.closes.iter().skip(1))
            .map(|(a, b)| ((b / a - 1.0) * 100.0) as f32)
            .collect();
        // Without its model the strategy can't do anything; the message says how to get one.
        let outputs = ml::run(MODEL, &ml::Tensor::row(&returns)).unwrap_or_else(|e| {
            panic!("{e}; `persephone-cli models example` writes a starter model")
        });
        let up = outputs[0].data[0];
        if up > self.threshold {
            Some(1)
        } else if up < 1.0 - self.threshold {
            Some(-1)
        } else {
            None
        }
    }
}

export_strategy!(MlTrend);

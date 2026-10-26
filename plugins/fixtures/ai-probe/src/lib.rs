//! Asks a language model every few bars, for the host's AI tests: LONG goes long, SHORT goes
//! short, anything else (or an error) keeps the position.

use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, ai, export_strategy};

struct AiProbe {
    every: u64,
    bars: u64,
}

impl Strategy for AiProbe {
    fn describe() -> StrategyInfo {
        StrategyInfo::new("AI probe", "Asks a model every few bars")
            .param(Param::period("Every", 24))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        Ok(Self {
            every: params[0] as u64,
            bars: 0,
        })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        self.bars += 1;
        if self.bars % self.every != 0 {
            return None;
        }
        let prompt = format!("Bar {} closed at {:.2}", self.bars, bar.close);
        let request = ai::Request::new("claude-opus-5-5", prompt).effort(ai::Effort::Low);
        match ai::complete(&request).as_deref().map(str::trim) {
            Ok("LONG") => Some(1),
            Ok("SHORT") => Some(-1),
            _ => None,
        }
    }
}

export_strategy!(AiProbe);

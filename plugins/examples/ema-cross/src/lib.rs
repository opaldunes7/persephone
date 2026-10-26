//! EMA crossover as a plugin: long on a golden cross, short on a death cross. Given the same
//! parameters it trades exactly like Persephone's built-in EMA Cross.

use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, export_strategy, ta::Ema};

struct EmaCross {
    fast: Ema,
    slow: Ema,
    fast_above: Option<bool>,
}

impl Strategy for EmaCross {
    fn describe() -> StrategyInfo {
        StrategyInfo::new("EMA Cross (plugin)", "Trend: follow fast/slow EMA crosses")
            .param(Param::period("Fast", 10))
            .param(Param::period("Slow", 30))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        let (fast, slow) = (params[0], params[1]);
        if fast >= slow {
            return Err(format!("Fast ({fast}) must be shorter than Slow ({slow})"));
        }
        Ok(Self {
            fast: Ema::new(fast as usize),
            slow: Ema::new(slow as usize),
            fast_above: None,
        })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        self.fast.update(bar.close);
        self.slow.update(bar.close);
        let (fast, slow) = (self.fast.value()?, self.slow.value()?);
        let above = fast > slow;
        let target = match self.fast_above {
            Some(false) if above => Some(1),
            Some(true) if !above => Some(-1),
            _ => None,
        };
        self.fast_above = Some(above);
        target
    }
}

export_strategy!(EmaCross);

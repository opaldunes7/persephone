//! The built-in strategies, as [`Signal`]s. Indicators come from Nautilus so they match what
//! the chart shows.

use std::collections::VecDeque;

use nautilus_indicators::{
    average::{MovingAverageType, ema::ExponentialMovingAverage},
    indicator::{Indicator, MovingAverage},
    momentum::rsi::RelativeStrengthIndex,
};
use nautilus_model::enums::PriceType;
use persephone_core::{
    Candle,
    strategy::{BuiltInStrategy, StrategySpec},
};

use super::Signal;

pub(crate) fn signal(spec: &StrategySpec) -> Box<dyn Signal> {
    match spec.kind {
        BuiltInStrategy::EmaCross => Box::new(EmaCross {
            fast: ExponentialMovingAverage::new(spec.period(0), Some(PriceType::Last)),
            slow: ExponentialMovingAverage::new(spec.period(1), Some(PriceType::Last)),
            fast_above: None,
        }),
        BuiltInStrategy::RsiReversion => Box::new(RsiReversion {
            rsi: RelativeStrengthIndex::new(spec.period(0), Some(MovingAverageType::Wilder)),
            oversold: spec.params[1],
            overbought: spec.params[2],
            previous: None,
        }),
        BuiltInStrategy::DonchianBreakout => Box::new(DonchianBreakout {
            length: spec.period(0),
            highs: VecDeque::new(),
            lows: VecDeque::new(),
        }),
    }
}

/// Long on a golden cross, short on a death cross.
struct EmaCross {
    fast: ExponentialMovingAverage,
    slow: ExponentialMovingAverage,
    fast_above: Option<bool>,
}

impl Signal for EmaCross {
    fn on_bar(&mut self, bar: &Candle) -> Option<i8> {
        self.fast.update_raw(bar.close);
        self.slow.update_raw(bar.close);
        if !self.fast.initialized() || !self.slow.initialized() {
            return None;
        }
        let above = self.fast.value() > self.slow.value();
        let target = match self.fast_above {
            Some(false) if above => Some(1),
            Some(true) if !above => Some(-1),
            _ => None,
        };
        self.fast_above = Some(above);
        target
    }
}

/// Long when RSI climbs back above oversold, short when it drops back below overbought.
struct RsiReversion {
    rsi: RelativeStrengthIndex,
    oversold: f64,
    overbought: f64,
    previous: Option<f64>,
}

impl Signal for RsiReversion {
    fn on_bar(&mut self, bar: &Candle) -> Option<i8> {
        self.rsi.update_raw(bar.close);
        if !self.rsi.initialized() {
            return None;
        }
        let value = self.rsi.value;
        let target = match self.previous {
            Some(p) if p < self.oversold && value >= self.oversold => Some(1),
            Some(p) if p > self.overbought && value <= self.overbought => Some(-1),
            _ => None,
        };
        self.previous = Some(value);
        target
    }
}

/// Long on a close above the previous `length` bars' highest high, short below their lowest
/// low.
struct DonchianBreakout {
    length: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
}

impl Signal for DonchianBreakout {
    fn on_bar(&mut self, bar: &Candle) -> Option<i8> {
        let target = (self.highs.len() == self.length).then(|| {
            let high = self.highs.iter().copied().fold(f64::MIN, f64::max);
            let low = self.lows.iter().copied().fold(f64::MAX, f64::min);
            if bar.close > high {
                Some(1)
            } else if bar.close < low {
                Some(-1)
            } else {
                None
            }
        });
        self.highs.push_back(bar.high);
        self.lows.push_back(bar.low);
        if self.highs.len() > self.length {
            self.highs.pop_front();
            self.lows.pop_front();
        }
        target.flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(close: f64) -> Candle {
        Candle {
            ts: 0,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 1.0,
        }
    }

    fn targets(spec: &StrategySpec, closes: impl IntoIterator<Item = f64>) -> Vec<i8> {
        let mut signal = signal(spec);
        closes
            .into_iter()
            .filter_map(|c| signal.on_bar(&bar(c)))
            .collect()
    }

    #[test]
    fn ema_cross_follows_the_trend_both_ways() {
        let spec = StrategySpec::with_params(BuiltInStrategy::EmaCross, &[3.0, 8.0, 10.0]);
        let down = (0..30).map(|i| 100.0 - f64::from(i));
        let up = (0..30).map(|i| 70.0 + 2.0 * f64::from(i));
        let fall = (0..30).map(|i| 130.0 - 3.0 * f64::from(i));
        assert_eq!(targets(&spec, down.chain(up).chain(fall)), vec![1, -1]);
    }

    #[test]
    fn rsi_reversion_fades_the_extremes() {
        let spec = StrategySpec::new(BuiltInStrategy::RsiReversion);
        // Sell off hard (oversold), recover, then rally hard (overbought) and roll over.
        let closes = (0..40)
            .map(|i| 100.0 - 1.5 * f64::from(i))
            .chain((0..60).map(|i| 40.0 + 2.0 * f64::from(i)))
            .chain((0..40).map(|i| 160.0 - 1.5 * f64::from(i)));
        let t = targets(&spec, closes);
        assert_eq!(t.first(), Some(&1), "{t:?}");
        assert!(t.contains(&-1), "{t:?}");
    }

    #[test]
    fn donchian_breaks_out_of_ranges() {
        let spec = StrategySpec::with_params(BuiltInStrategy::DonchianBreakout, &[5.0, 10.0]);
        // A range, a breakout to 110, a pause there (inside its own bars), then a crash.
        let range = [100.0, 101.0, 99.0, 100.0, 101.0, 100.0];
        let closes = range.into_iter().chain([110.0; 6]).chain([80.0]);
        assert_eq!(targets(&spec, closes), vec![1, -1]);
    }
}

//! Built-in indicator values, computed with Nautilus's own indicators so the chart shows
//! exactly what a strategy running in the engine sees.

use nautilus_indicators::{
    average::{MovingAverageType, ema::ExponentialMovingAverage, sma::SimpleMovingAverage},
    indicator::{Indicator, MovingAverage},
    momentum::{
        bb::BollingerBands, macd::MovingAverageConvergenceDivergence, rsi::RelativeStrengthIndex,
        stochastics::Stochastics,
    },
    volatility::{atr::AverageTrueRange, dc::DonchianChannel},
};
use nautilus_model::enums::PriceType;
use persephone_core::{
    Candle,
    indicator::{BuiltIn, IndicatorKind, IndicatorSpec},
};

/// One series per output of a built-in indicator, each as long as `candles`. Bars before the
/// indicator has enough history are `NaN`.
///
/// # Errors
///
/// Returns a message when the parameters don't make sense together (MACD's fast period must
/// be shorter than its slow period), or for a plugin, which the plugin host computes.
pub fn compute(spec: &IndicatorSpec, candles: &[Candle]) -> Result<Vec<Vec<f64>>, String> {
    let kind = match &spec.kind {
        IndicatorKind::BuiltIn(kind) => *kind,
        IndicatorKind::Plugin(id) => {
            return Err(format!("`{id}` is a plugin; the plugin host computes it"));
        }
    };
    let mut out = vec![vec![f64::NAN; candles.len()]; spec.info.outputs.len()];
    let p = |i| spec.period(i);

    match kind {
        BuiltIn::Sma => {
            let mut sma = SimpleMovingAverage::new(p(0), Some(PriceType::Last));
            fill(&mut out, candles, |c| {
                sma.update_raw(c.close);
                sma.initialized().then(|| vec![sma.value()])
            });
        }
        BuiltIn::Ema => {
            let mut ema = ExponentialMovingAverage::new(p(0), Some(PriceType::Last));
            fill(&mut out, candles, |c| {
                ema.update_raw(c.close);
                ema.initialized().then(|| vec![ema.value()])
            });
        }
        BuiltIn::Bollinger => {
            let mut bb = BollingerBands::new(p(0), spec.params[1], Some(MovingAverageType::Simple));
            fill(&mut out, candles, |c| {
                bb.update_raw(c.close);
                bb.initialized()
                    .then(|| vec![bb.upper, bb.middle, bb.lower])
            });
        }
        BuiltIn::Donchian => {
            let mut dc = DonchianChannel::new(p(0));
            fill(&mut out, candles, |c| {
                dc.update_raw(c.high, c.low);
                dc.initialized()
                    .then(|| vec![dc.upper, dc.middle, dc.lower])
            });
        }
        BuiltIn::Rsi => {
            // Wilder smoothing, as TradingView's RSI uses.
            let mut rsi = RelativeStrengthIndex::new(p(0), Some(MovingAverageType::Wilder));
            fill(&mut out, candles, |c| {
                rsi.update_raw(c.close);
                rsi.initialized().then(|| vec![rsi.value])
            });
        }
        BuiltIn::Macd => {
            let (fast, slow, signal) = (p(0), p(1), p(2));
            if fast >= slow {
                return Err(format!(
                    "MACD fast ({fast}) must be shorter than slow ({slow})"
                ));
            }
            let mut macd = MovingAverageConvergenceDivergence::new(
                fast,
                slow,
                Some(signal),
                Some(MovingAverageType::Exponential),
                Some(PriceType::Last),
            );
            fill(&mut out, candles, |c| {
                macd.update_raw(c.close);
                macd.initialized()
                    .then(|| vec![macd.histogram, macd.value, macd.signal])
            });
        }
        BuiltIn::Stochastic => {
            let mut stoch = Stochastics::new(p(0), p(1));
            fill(&mut out, candles, |c| {
                stoch.update_raw(c.high, c.low, c.close);
                stoch
                    .initialized()
                    .then(|| vec![stoch.value_k, stoch.value_d])
            });
        }
        BuiltIn::Atr => {
            let mut atr =
                AverageTrueRange::new(p(0), Some(MovingAverageType::Wilder), Some(true), Some(0.0));
            fill(&mut out, candles, |c| {
                atr.update_raw(c.high, c.low, c.close);
                atr.initialized().then(|| vec![atr.value])
            });
        }
    }
    Ok(out)
}

/// A built-in indicator fed one value at a time, for rules built while the app runs. Being
/// Nautilus's own, it gives the same values as [`compute`], and can run over any series.
pub enum Streaming {
    Sma(SimpleMovingAverage),
    Ema(ExponentialMovingAverage),
    Rsi(RelativeStrengthIndex),
    Atr(AverageTrueRange),
}

impl Streaming {
    #[must_use]
    pub fn sma(period: usize) -> Self {
        Self::Sma(SimpleMovingAverage::new(period, Some(PriceType::Last)))
    }

    #[must_use]
    pub fn ema(period: usize) -> Self {
        Self::Ema(ExponentialMovingAverage::new(period, Some(PriceType::Last)))
    }

    /// With Wilder smoothing, like [`BuiltIn::Rsi`].
    #[must_use]
    pub fn rsi(period: usize) -> Self {
        Self::Rsi(RelativeStrengthIndex::new(
            period,
            Some(MovingAverageType::Wilder),
        ))
    }

    /// With Wilder smoothing, like [`BuiltIn::Atr`].
    #[must_use]
    pub fn atr(period: usize) -> Self {
        Self::Atr(AverageTrueRange::new(
            period,
            Some(MovingAverageType::Wilder),
            Some(true),
            Some(0.0),
        ))
    }

    /// Takes the next value (ATR's is the bar's), returning the indicator's once warmed up.
    pub fn update(&mut self, value: f64, bar: &Candle) -> Option<f64> {
        match self {
            Self::Sma(sma) => {
                sma.update_raw(value);
                sma.initialized().then(|| sma.value())
            }
            Self::Ema(ema) => {
                ema.update_raw(value);
                ema.initialized().then(|| ema.value())
            }
            Self::Rsi(rsi) => {
                rsi.update_raw(value);
                rsi.initialized().then_some(rsi.value)
            }
            Self::Atr(atr) => {
                atr.update_raw(bar.high, bar.low, bar.close);
                atr.initialized().then_some(atr.value)
            }
        }
    }
}

/// Feeds every candle to `step` and stores the values it returns once warmed up.
fn fill(
    out: &mut [Vec<f64>],
    candles: &[Candle],
    mut step: impl FnMut(&Candle) -> Option<Vec<f64>>,
) {
    for (i, candle) in candles.iter().enumerate() {
        if let Some(values) = step(candle) {
            for (series, value) in out.iter_mut().zip(values) {
                series[i] = value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(closes: &[f64]) -> Vec<Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Candle {
                ts: i as u64,
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 1.0,
            })
            .collect()
    }

    fn wave(n: usize) -> Vec<Candle> {
        candles(
            &(0..n)
                .map(|i| 100.0 + 10.0 * (i as f64 / 7.0).sin())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn sma_matches_a_hand_average() {
        let data = candles(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let out = compute(&IndicatorSpec::with_params(BuiltIn::Sma, &[3.0]), &data).unwrap();
        assert!(out[0][0].is_nan() && out[0][1].is_nan(), "warming up");
        assert_eq!(&out[0][2..], &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn every_builtin_produces_aligned_outputs() {
        let data = wave(300);
        for kind in BuiltIn::ALL {
            let out = compute(&IndicatorSpec::new(kind), &data).unwrap();
            assert_eq!(out.len(), kind.outputs().len(), "{kind:?}");
            for series in &out {
                assert_eq!(series.len(), data.len());
                assert!(
                    series[250..].iter().all(|v| v.is_finite()),
                    "{kind:?} warms up"
                );
            }
        }
    }

    #[test]
    fn oscillators_stay_in_range_and_bands_bracket_the_basis() {
        let data = wave(300);
        let rsi = &compute(&IndicatorSpec::new(BuiltIn::Rsi), &data).unwrap()[0];
        assert!(
            rsi.iter()
                .filter(|v| v.is_finite())
                .all(|v| (0.0..=100.0).contains(v))
        );

        let bb = compute(&IndicatorSpec::new(BuiltIn::Bollinger), &data).unwrap();
        let bands = bb[0].iter().zip(&bb[1]).zip(&bb[2]).skip(50);
        assert!(
            bands
                .into_iter()
                .all(|((upper, basis), lower)| upper >= basis && basis >= lower)
        );

        let macd = compute(&IndicatorSpec::new(BuiltIn::Macd), &data).unwrap();
        let parts = macd[0].iter().zip(&macd[1]).zip(&macd[2]).skip(100);
        for ((histogram, line), signal) in parts {
            assert!(
                (histogram - (line - signal)).abs() < 1e-9,
                "histogram = macd - signal"
            );
        }
    }

    #[test]
    fn macd_rejects_fast_slower_than_slow() {
        let spec = IndicatorSpec::with_params(BuiltIn::Macd, &[30.0, 10.0, 9.0]);
        assert!(compute(&spec, &wave(100)).is_err());
    }
}

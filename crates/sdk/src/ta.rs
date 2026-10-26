//! Small technical-analysis helpers for plugins. They follow NautilusTrader's formulas, so a
//! plugin built on them computes what Persephone's built-ins do.

/// Simple moving average of every `period` values; `NaN` until `period` values are in.
#[must_use]
pub fn sma(values: &[f64], period: usize) -> Vec<f64> {
    let period = period.max(1);
    let mut out = vec![f64::NAN; values.len()];
    let mut sum = 0.0;
    for (i, value) in values.iter().enumerate() {
        sum += value;
        if i >= period {
            sum -= values[i - period];
        }
        if i + 1 >= period {
            out[i] = sum / period as f64;
        }
    }
    out
}

/// Exponential moving average over a series; `NaN` until it is warmed up (see [`Ema`]).
#[must_use]
pub fn ema(values: &[f64], period: usize) -> Vec<f64> {
    let mut ema = Ema::new(period);
    values
        .iter()
        .map(|&v| {
            ema.update(v);
            ema.value().unwrap_or(f64::NAN)
        })
        .collect()
}

/// An exponential moving average fed one value at a time. Seeded with the simple average of
/// its first `period` values, then smoothed with `alpha = 2 / (period + 1)`.
#[derive(Clone, Debug)]
pub struct Ema {
    period: usize,
    alpha: f64,
    count: usize,
    seed: f64,
    value: f64,
}

impl Ema {
    #[must_use]
    pub fn new(period: usize) -> Self {
        let period = period.max(1);
        Self {
            period,
            alpha: 2.0 / (period as f64 + 1.0),
            count: 0,
            seed: 0.0,
            value: 0.0,
        }
    }

    pub fn update(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.count += 1;
        if self.count < self.period {
            self.seed += value;
        } else if self.count == self.period {
            self.seed += value;
            self.value = self.seed / self.period as f64;
        } else {
            self.value = self.alpha.mul_add(value, (1.0 - self.alpha) * self.value);
        }
    }

    /// The current value, once `period` values are in.
    #[must_use]
    pub fn value(&self) -> Option<f64> {
        (self.count >= self.period).then_some(self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_warms_up_then_averages() {
        let out = sma(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert!(out[0].is_nan() && out[1].is_nan());
        assert_eq!(&out[2..], &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn ema_is_seeded_with_the_simple_average() {
        let mut average = Ema::new(3);
        average.update(1.0);
        average.update(2.0);
        assert_eq!(average.value(), None);
        average.update(3.0);
        assert_eq!(average.value(), Some(2.0));
        average.update(6.0);
        assert_eq!(average.value(), Some(4.0), "0.5 * 6 + 0.5 * 2");
        assert_eq!(ema(&[1.0, 2.0, 3.0, 6.0], 3)[3], 4.0);
    }
}

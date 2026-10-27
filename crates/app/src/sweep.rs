//! Parameter sweeps: one strategy, one parameter, several values, a chart for each, side by
//! side on a workspace of their own.

use persephone_core::indicator::Param;
use persephone_tiling::TileId;

/// The most charts a sweep opens.
pub const MAX_CHARTS: usize = 9;

/// A sweep in progress: the name of the parameter it varies and the chart showing each value.
#[derive(Clone, Debug)]
pub struct Sweep {
    pub name: String,
    pub charts: Vec<(TileId, f64)>,
}

/// `charts` values from `from` to `to`, evenly spaced, rounded for whole-number parameters,
/// without repeats.
///
/// # Errors
///
/// Returns a message if the count or the ends don't suit the parameter, or rounding leaves
/// fewer than two values.
pub fn values(param: &Param, from: f64, to: f64, charts: usize) -> Result<Vec<f64>, String> {
    if !(2..=MAX_CHARTS).contains(&charts) {
        return Err(format!("a sweep opens 2 to {MAX_CHARTS} charts"));
    }
    let round = |v: f64| {
        if param.integer {
            v.round()
        } else {
            (v * 10_000.0).round() / 10_000.0
        }
    };
    param.check(round(from))?;
    param.check(round(to))?;
    let mut out: Vec<f64> = Vec::with_capacity(charts);
    for k in 0..charts {
        let value = round(from + (to - from) * k as f64 / (charts - 1) as f64);
        if !out.contains(&value) {
            out.push(value);
        }
    }
    if out.len() < 2 {
        return Err("those give fewer than two different values".into());
    }
    Ok(out)
}

/// A starting range around `value`: half to double, within the parameter's limits.
#[must_use]
pub fn default_range(param: &Param, value: f64) -> (f64, f64) {
    let round = |v: f64| if param.integer { v.round() } else { v };
    (
        round((value / 2.0).max(param.min)),
        round((value * 2.0).min(param.max)),
    )
}

/// How a number reads in a sweep: whole numbers without decimals.
#[must_use]
pub fn label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn period() -> Param {
        Param::period("Fast", 10.0)
    }

    #[test]
    fn values_spread_evenly_and_round_for_whole_numbers() {
        assert_eq!(
            values(&period(), 5.0, 20.0, 4).unwrap(),
            vec![5.0, 10.0, 15.0, 20.0]
        );
        assert_eq!(
            values(&period(), 5.0, 6.0, 4).unwrap(),
            vec![5.0, 6.0],
            "repeats dropped"
        );
        let size = Param {
            name: "Size %".into(),
            default: 10.0,
            min: 1.0,
            max: 100.0,
            integer: false,
        };
        assert_eq!(values(&size, 5.0, 10.0, 3).unwrap(), vec![5.0, 7.5, 10.0]);
    }

    #[test]
    fn says_what_doesnt_fit() {
        assert!(
            values(&period(), 5.0, 20.0, 1)
                .unwrap_err()
                .contains("2 to 9")
        );
        assert!(
            values(&period(), 0.0, 20.0, 3).is_err(),
            "below the minimum"
        );
        assert!(
            values(&period(), 5.0, 5.0, 3)
                .unwrap_err()
                .contains("fewer than two")
        );
    }

    #[test]
    fn default_ranges_halve_and_double_within_limits() {
        assert_eq!(default_range(&period(), 10.0), (5.0, 20.0));
        assert_eq!(default_range(&period(), 1.0), (1.0, 2.0));
        assert_eq!(label(15.0), "15");
        assert_eq!(label(7.5), "7.5");
    }
}

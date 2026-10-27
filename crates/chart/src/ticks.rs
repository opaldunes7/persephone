//! Axis tick placement and labels.

use persephone_core::{
    BarSpec, Candle, Timeframe,
    time::{DateTime, NANOS_PER_DAY, NANOS_PER_MINUTE},
};

use crate::viewport::Projection;

/// Minimum vertical gap between price labels, in pixels: tighter in small tiles so they
/// still get a few labels, looser on big charts so they don't get crowded.
fn price_gap(height: f32) -> f32 {
    (height / 6.0).clamp(30.0, 56.0)
}
/// Minimum horizontal gap between time labels, in pixels.
const TIME_GAP: f32 = 90.0;

/// Round price levels at least [`price_gap`] pixels apart, bottom to top.
#[must_use]
pub fn price_ticks(projection: &Projection) -> Vec<f64> {
    let range = projection.top - projection.bottom;
    if range <= 0.0 || projection.height <= 0.0 {
        return Vec::new();
    }
    let gap = price_gap(projection.height);
    let step = nice_step(range * f64::from(gap) / f64::from(projection.height));
    let first = (projection.bottom / step).ceil() as i64;
    let last = (projection.top / step).floor() as i64;
    (first..=last).map(|k| k as f64 * step).collect()
}

/// The smallest 1/2/2.5/5 × 10ⁿ step that is at least `min_step`.
fn nice_step(min_step: f64) -> f64 {
    let magnitude = 10f64.powf(min_step.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= min_step * (1.0 - 1e-9))
        .unwrap_or(10.0 * magnitude)
}

/// A labelled bar on the time axis.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeTick {
    pub index: usize,
    pub label: String,
    /// Day, month or year boundaries are drawn brighter.
    pub major: bool,
}

/// Bucket sizes for time labels, from finest to coarsest.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Interval {
    Nanos(u64),
    Month,
    Year,
}

impl Interval {
    const ALL: [Self; 16] = [
        Self::Nanos(NANOS_PER_MINUTE),
        Self::Nanos(5 * NANOS_PER_MINUTE),
        Self::Nanos(15 * NANOS_PER_MINUTE),
        Self::Nanos(30 * NANOS_PER_MINUTE),
        Self::Nanos(60 * NANOS_PER_MINUTE),
        Self::Nanos(2 * 60 * NANOS_PER_MINUTE),
        Self::Nanos(3 * 60 * NANOS_PER_MINUTE),
        Self::Nanos(4 * 60 * NANOS_PER_MINUTE),
        Self::Nanos(6 * 60 * NANOS_PER_MINUTE),
        Self::Nanos(12 * 60 * NANOS_PER_MINUTE),
        Self::Nanos(NANOS_PER_DAY),
        Self::Nanos(2 * NANOS_PER_DAY),
        Self::Nanos(7 * NANOS_PER_DAY),
        Self::Nanos(14 * NANOS_PER_DAY),
        Self::Month,
        Self::Year,
    ];

    fn approx_nanos(self) -> u64 {
        match self {
            Self::Nanos(n) => n,
            Self::Month => 30 * NANOS_PER_DAY,
            Self::Year => 365 * NANOS_PER_DAY,
        }
    }

    fn bucket(self, ts: u64) -> i64 {
        match self {
            Self::Nanos(n) => (ts / n) as i64,
            Self::Month => {
                let dt = DateTime::from_nanos(ts);
                dt.year * 12 + i64::from(dt.month)
            }
            Self::Year => DateTime::from_nanos(ts).year,
        }
    }
}

/// Labels for the bars where a time bucket starts, at least [`TIME_GAP`] pixels apart.
///
/// Month and year boundaries always compete for a spot and win over finer ones, as on
/// TradingView, so a daily chart reads "Jul … 14 … Aug" instead of arbitrary two-week marks.
#[must_use]
pub fn time_ticks(candles: &[Candle], projection: &Projection, bar_nanos: u64) -> Vec<TimeTick> {
    let bars_per_gap = f64::from(TIME_GAP) / projection.spacing;
    let min_nanos = (bars_per_gap * bar_nanos as f64) as u64;
    let interval = Interval::ALL
        .into_iter()
        .find(|i| i.approx_nanos() >= min_nanos.max(bar_nanos))
        .unwrap_or(Interval::Year);

    let start = projection.visible.start.max(1);
    let mut candidates: Vec<(Boundary, usize)> = (start..projection.visible.end)
        .filter_map(|index| {
            let (prev, cur) = (candles[index - 1].ts, candles[index].ts);
            let boundary = Boundary::between(prev, cur);
            let eligible =
                boundary >= Boundary::Month || interval.bucket(prev) != interval.bucket(cur);
            eligible.then_some((boundary, index))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let mut placed: Vec<(Boundary, usize)> = Vec::new();
    for (boundary, index) in candidates {
        let x = projection.x(index as f64);
        if placed
            .iter()
            .all(|(_, other)| (x - projection.x(*other as f64)).abs() >= TIME_GAP)
        {
            placed.push((boundary, index));
        }
    }
    placed.sort_by_key(|(_, index)| *index);

    placed
        .into_iter()
        .map(|(boundary, index)| TimeTick {
            index,
            label: boundary.label(candles[index].ts),
            major: boundary >= Boundary::Day,
        })
        .collect()
}

/// The largest calendar unit that changes between two consecutive bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Boundary {
    Time,
    Day,
    Month,
    Year,
}

impl Boundary {
    fn between(prev: u64, cur: u64) -> Self {
        let (p, c) = (DateTime::from_nanos(prev), DateTime::from_nanos(cur));
        if c.year != p.year {
            Self::Year
        } else if c.month != p.month {
            Self::Month
        } else if c.day != p.day {
            Self::Day
        } else {
            Self::Time
        }
    }

    /// TradingView-style label: the year at a new year, the month at a new month, the day
    /// number at a new day, otherwise the time.
    fn label(self, ts: u64) -> String {
        let dt = DateTime::from_nanos(ts);
        match self {
            Self::Year => dt.year.to_string(),
            Self::Month => dt.month_name().to_owned(),
            Self::Day => dt.day.to_string(),
            Self::Time => dt.time(),
        }
    }
}

/// Full timestamp for the crosshair label, e.g. `Sun 25 Oct '26 17:00`, or `Oct '26` for
/// monthly bars.
#[must_use]
pub fn crosshair_label(ts: u64, bars: BarSpec) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let dt = DateTime::from_nanos(ts);
    let weekday = WEEKDAYS[((ts / NANOS_PER_DAY) % 7) as usize];
    let date = format!(
        "{weekday} {:02} {} '{:02}",
        dt.day,
        dt.month_name(),
        dt.year % 100
    );
    match bars {
        BarSpec::Time(Timeframe::MN1) => format!("{} '{:02}", dt.month_name(), dt.year % 100),
        BarSpec::Time(Timeframe::D1 | Timeframe::W1) => date,
        BarSpec::Time(_) => format!("{date}  {}", dt.time()),
        // Tick and volume bars can be seconds apart.
        BarSpec::Ticks(_) | BarSpec::Volume(_) => {
            format!("{date}  {}:{:02}", dt.time(), ts / 1_000_000_000 % 60)
        }
    }
}

/// Formats a price with the instrument's precision.
#[must_use]
pub fn format_price(price: f64, precision: u8) -> String {
    format!("{price:.*}", usize::from(precision))
}

/// Formats an indicator value: two decimals, four for values below one.
#[must_use]
pub fn format_value(value: f64) -> String {
    if !value.is_finite() {
        "—".into()
    } else if value.abs() >= 1.0 || value == 0.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    }
}

/// Decimal places needed to show labels `step` apart (e.g. 2.5 → 1, 0.05 → 2, 20 → 0).
#[must_use]
pub fn step_decimals(step: f64) -> usize {
    let text = format!("{step:.8}");
    text.trim_end_matches('0')
        .split_once('.')
        .map_or(0, |(_, frac)| frac.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::Viewport;

    #[test]
    fn small_tiles_still_get_several_price_labels() {
        let data: Vec<Candle> = (0..2_000u64)
            .map(|i| {
                let p = 80_480.0 + 160.0 * (i as f64 / 30.0).sin().abs();
                Candle {
                    ts: i,
                    open: p,
                    high: p + 10.0,
                    low: p - 10.0,
                    close: p + 3.0,
                    volume: 1.0,
                }
            })
            .collect();
        // A tile in a 4x4 grid on a 1440x900 window.
        let p = Projection::new(&Viewport::new(data.len()), &data, 270.0, 187.0);
        let inside = price_ticks(&p)
            .iter()
            .filter(|t| (8.0..179.0).contains(&p.y(**t)))
            .count();
        assert!(inside >= 3, "only {inside} labels");
    }

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(0.7), 1.0);
        assert_eq!(nice_step(1.3), 2.0);
        assert_eq!(nice_step(2.2), 2.5);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(130.0), 200.0);
        assert!((nice_step(0.013) - 0.02).abs() < 1e-12);
    }

    fn hourly(n: usize, start: u64) -> Vec<Candle> {
        (0..n)
            .map(|i| Candle {
                ts: start + i as u64 * Timeframe::H1.nanos(),
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: 1.0,
            })
            .collect()
    }

    #[test]
    fn price_ticks_are_round_and_spaced() {
        let data = hourly(100, 0);
        let p = Projection::new(&Viewport::new(100), &data, 800.0, 400.0);
        let ticks = price_ticks(&p);
        assert!(ticks.len() >= 3, "{ticks:?}");
        let gap = p.y(ticks[0]) - p.y(ticks[1]);
        assert!(gap >= price_gap(400.0) - 1e-3, "gap {gap}");
        assert!(ticks.iter().all(|t| *t >= p.bottom && *t <= p.top));
    }

    #[test]
    fn time_ticks_mark_day_boundaries_and_keep_their_distance() {
        // 2026-10-20 00:00 UTC, five days of hourly bars.
        let start = 1_792_454_400_000_000_000;
        let data = hourly(120, start);
        let mut vp = Viewport::new(data.len());
        vp.spacing = 6.0;
        let p = Projection::new(&vp, &data, 800.0, 400.0);
        let ticks = time_ticks(&data, &p, Timeframe::H1.nanos());

        assert!(!ticks.is_empty());
        assert!(
            ticks.iter().any(|t| t.major && t.label == "21"),
            "{ticks:?}"
        );
        for pair in ticks.windows(2) {
            assert!(p.x(pair[1].index as f64) - p.x(pair[0].index as f64) >= TIME_GAP);
        }
    }

    #[test]
    fn labels_prefer_the_largest_boundary() {
        let label = |prev, cur| {
            let boundary = Boundary::between(prev, cur);
            (boundary.label(cur), boundary >= Boundary::Day)
        };
        let day = NANOS_PER_DAY;
        let jan1_2027 = 1_798_761_600_000_000_000;
        assert_eq!(label(jan1_2027 - day, jan1_2027), ("2027".into(), true));
        let nov1_2026 = 1_793_491_200_000_000_000;
        assert_eq!(
            label(nov1_2026 - 3_600_000_000_000, nov1_2026),
            ("Nov".into(), true)
        );
        assert_eq!(label(nov1_2026, nov1_2026 + day), ("2".into(), true));
        let h = Timeframe::H1.nanos();
        assert_eq!(
            label(nov1_2026 + 13 * h, nov1_2026 + 14 * h),
            ("14:00".into(), false)
        );
    }

    #[test]
    fn daily_charts_label_months() {
        // 2026-03-01 onward, 240 daily bars: several month starts on screen.
        let start = 1_772_323_200_000_000_000;
        let data: Vec<Candle> = (0..240)
            .map(|i| Candle {
                ts: start + i * NANOS_PER_DAY,
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: 1.0,
            })
            .collect();
        let mut vp = Viewport::new(data.len());
        vp.spacing = 6.0;
        let p = Projection::new(&vp, &data, 1_200.0, 400.0);
        let ticks = time_ticks(&data, &p, Timeframe::D1.nanos());
        let labels: Vec<_> = ticks.iter().map(|t| t.label.as_str()).collect();

        for month in ["Jun", "Jul", "Aug", "Sep"] {
            assert!(labels.contains(&month), "{labels:?}");
        }
        for t in &ticks {
            if t.label.len() == 3 {
                assert_eq!(
                    DateTime::from_nanos(data[t.index].ts).day,
                    1,
                    "month label on the 1st"
                );
            }
        }
        for pair in ticks.windows(2) {
            assert!(p.x(pair[1].index as f64) - p.x(pair[0].index as f64) >= TIME_GAP);
        }
    }

    #[test]
    fn value_formats() {
        assert_eq!(format_value(55.234), "55.23");
        assert_eq!(format_value(-0.01234), "-0.0123");
        assert_eq!(format_value(f64::NAN), "—");
        assert_eq!(step_decimals(2.5), 1);
        assert_eq!(step_decimals(0.05), 2);
        assert_eq!(step_decimals(20.0), 0);
    }

    #[test]
    fn crosshair_labels() {
        assert_eq!(
            crosshair_label(1_792_947_600_000_000_000, Timeframe::H1.into()),
            "Sun 25 Oct '26  17:00"
        );
        assert_eq!(
            crosshair_label(1_792_886_400_000_000_000, Timeframe::D1.into()),
            "Sun 25 Oct '26"
        );
        assert_eq!(
            crosshair_label(1_792_886_400_000_000_000, Timeframe::MN1.into()),
            "Oct '26"
        );
        assert_eq!(format_price(80_806.014, 2), "80806.01");
    }
}

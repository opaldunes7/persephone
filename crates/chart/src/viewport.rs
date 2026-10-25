//! Viewport state and the bar/price ↔ pixel projection.
//!
//! The x-axis counts bars, not clock time (as TradingView does), so gaps in the data never
//! leave holes in the chart. Bar `i` is centered at x = `(i - left) * spacing`.

use std::ops::Range;

use persephone_core::Candle;

pub const MIN_SPACING: f64 = 1.0;
pub const MAX_SPACING: f64 = 60.0;
pub const DEFAULT_SPACING: f64 = 8.0;
/// Empty bars kept to the right of the last bar at startup.
pub const RIGHT_MARGIN_BARS: f64 = 6.0;
/// Fraction of the plot height above the highest visible high.
pub const TOP_MARGIN: f64 = 0.08;
/// Fraction of the plot height below the lowest visible low; the volume histogram lives here.
pub const BOTTOM_MARGIN: f64 = 0.22;
/// Height of the tallest visible volume bar, as a fraction of the plot height.
pub const VOLUME_HEIGHT: f32 = 0.18;

/// What the user controls: horizontal position and zoom, plus price-axis scaling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Bar index (fractional) at the plot's right edge.
    pub right: f64,
    /// Pixels per bar.
    pub spacing: f64,
    /// Multiplies the auto-fitted price range: 1 fits the visible bars, >1 shrinks them.
    pub price_zoom: f64,
}

impl Viewport {
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            right: len as f64 - 1.0 + RIGHT_MARGIN_BARS,
            spacing: DEFAULT_SPACING,
            price_zoom: 1.0,
        }
    }

    /// Drags the chart by `dx` pixels (positive moves bars to the right, into the past).
    pub fn pan(&mut self, dx: f32, plot_width: f32, len: usize) {
        self.right -= f64::from(dx) / self.spacing;
        self.clamp(plot_width, len);
    }

    /// Zooms by `factor` (>1 zooms in) keeping the bar `from_right` pixels left of the right
    /// edge under the cursor.
    pub fn zoom(&mut self, factor: f32, from_right: f32, plot_width: f32, len: usize) {
        let from_right = f64::from(from_right);
        let anchor = self.right - from_right / self.spacing;
        self.spacing = (self.spacing * f64::from(factor)).clamp(MIN_SPACING, MAX_SPACING);
        self.right = anchor + from_right / self.spacing;
        self.clamp(plot_width, len);
    }

    /// Dragging the price axis down by `dy` pixels compresses the candles.
    pub fn scale_price(&mut self, dy: f32) {
        self.price_zoom = (self.price_zoom * (f64::from(dy) * 0.005).exp()).clamp(0.05, 20.0);
    }

    pub fn reset_price(&mut self) {
        self.price_zoom = 1.0;
    }

    /// Keeps a few bars on screen at either end of the data.
    fn clamp(&mut self, plot_width: f32, len: usize) {
        let last = len.saturating_sub(1) as f64;
        let visible = f64::from(plot_width) / self.spacing;
        let min_right = last.min(4.0);
        let max_right = last + (visible * 0.5).max(RIGHT_MARGIN_BARS);
        self.right = self.right.clamp(min_right, max_right);
    }
}

/// The mapping between bar index/price and plot pixels for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// Bar index at x = 0.
    pub left: f64,
    /// Pixels per bar.
    pub spacing: f64,
    /// Price at y = 0.
    pub top: f64,
    /// Price at y = `height`.
    pub bottom: f64,
    pub width: f32,
    pub height: f32,
    /// Bars with any part on screen.
    pub visible: Range<usize>,
    /// Largest volume among visible bars.
    pub max_volume: f64,
}

impl Projection {
    #[must_use]
    pub fn new(viewport: &Viewport, candles: &[Candle], width: f32, height: f32) -> Self {
        let spacing = viewport.spacing;
        let left = viewport.right - f64::from(width) / spacing;
        let visible = visible_range(left, viewport.right, candles.len());

        let fit = if visible.is_empty() {
            candles.last().map(|c| (c.low, c.high, c.volume))
        } else {
            candles[visible.clone()].iter().fold(None, |acc, c| {
                Some(match acc {
                    None => (c.low, c.high, c.volume),
                    Some((lo, hi, vol)) => (c.low.min(lo), c.high.max(hi), c.volume.max(vol)),
                })
            })
        };
        let (lo, hi, max_volume) = fit.unwrap_or((0.0, 1.0, 0.0));

        // A flat range would divide by zero; give it a little height around the price.
        let half = ((hi - lo) / 2.0).max(hi.abs() * 1e-4).max(1e-9) * viewport.price_zoom;
        let mid = (hi + lo) / 2.0;
        let span = 2.0 * half;
        let inner = 1.0 - TOP_MARGIN - BOTTOM_MARGIN;

        Self {
            left,
            spacing,
            top: mid + half + span * TOP_MARGIN / inner,
            bottom: mid - half - span * BOTTOM_MARGIN / inner,
            width,
            height,
            visible,
            max_volume,
        }
    }

    /// X of bar `index`'s center.
    #[must_use]
    pub fn x(&self, index: f64) -> f32 {
        ((index - self.left) * self.spacing) as f32
    }

    /// Fractional bar index at `x`.
    #[must_use]
    pub fn index_at(&self, x: f32) -> f64 {
        self.left + f64::from(x) / self.spacing
    }

    /// The bar nearest to `x`, if there is one.
    #[must_use]
    pub fn bar_at(&self, x: f32, len: usize) -> Option<usize> {
        let index = self.index_at(x).round();
        (index >= 0.0 && index < len as f64).then_some(index as usize)
    }

    #[must_use]
    pub fn y(&self, price: f64) -> f32 {
        ((self.top - price) / (self.top - self.bottom) * f64::from(self.height)) as f32
    }

    #[must_use]
    pub fn price_at(&self, y: f32) -> f64 {
        self.top - f64::from(y) / f64::from(self.height) * (self.top - self.bottom)
    }
}

/// Bars whose center lies within half a bar of `[left, right]`.
fn visible_range(left: f64, right: f64, len: usize) -> Range<usize> {
    let start = (left - 0.5).ceil().max(0.0) as usize;
    let end = ((right + 0.5).floor() + 1.0).max(0.0) as usize;
    let end = end.min(len);
    start.min(end)..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(n: usize) -> Vec<Candle> {
        (0..n)
            .map(|i| {
                let p = 100.0 + i as f64;
                Candle {
                    ts: i as u64,
                    open: p,
                    high: p + 2.0,
                    low: p - 2.0,
                    close: p + 1.0,
                    volume: 10.0 + i as f64,
                }
            })
            .collect()
    }

    #[test]
    fn x_and_index_round_trip() {
        let data = candles(500);
        let p = Projection::new(&Viewport::new(500), &data, 800.0, 400.0);
        for x in [0.0, 13.5, 400.0, 799.0] {
            assert!((p.x(p.index_at(x)) - x).abs() < 1e-3);
        }
        // The last bar sits RIGHT_MARGIN_BARS bars left of the right edge.
        let last_x = p.x(499.0);
        assert!((800.0 - last_x - (RIGHT_MARGIN_BARS * DEFAULT_SPACING) as f32).abs() < 1e-3);
    }

    #[test]
    fn y_and_price_round_trip() {
        let data = candles(200);
        let p = Projection::new(&Viewport::new(200), &data, 800.0, 400.0);
        for y in [0.0, 100.0, 399.0] {
            assert!((p.y(p.price_at(y)) - y).abs() < 1e-3);
        }
    }

    #[test]
    fn fit_frames_visible_bars_between_the_margins() {
        let data = candles(1_000);
        let p = Projection::new(&Viewport::new(1_000), &data, 800.0, 400.0);
        let hi = data[p.visible.clone()]
            .iter()
            .map(|c| c.high)
            .fold(f64::MIN, f64::max);
        let lo = data[p.visible.clone()]
            .iter()
            .map(|c| c.low)
            .fold(f64::MAX, f64::min);
        assert!((p.y(hi) - 400.0 * TOP_MARGIN as f32).abs() < 1e-2);
        assert!((p.y(lo) - 400.0 * (1.0 - BOTTOM_MARGIN) as f32).abs() < 1e-2);
        assert_eq!(p.max_volume, data[p.visible.end - 1].volume);
    }

    #[test]
    fn visible_range_covers_partial_bars() {
        assert_eq!(visible_range(10.2, 20.7, 100), 10..22);
        assert_eq!(visible_range(-5.0, 3.0, 100), 0..4);
        assert_eq!(visible_range(95.0, 120.0, 100), 95..100);
        assert!(visible_range(150.0, 160.0, 100).is_empty());
    }

    #[test]
    fn zoom_keeps_the_bar_under_the_cursor() {
        let data = candles(2_000);
        let mut vp = Viewport::new(2_000);
        vp.pan(400.0, 800.0, data.len()); // away from the clamp
        let before = Projection::new(&vp, &data, 800.0, 400.0).index_at(300.0);
        vp.zoom(1.6, 500.0, 800.0, data.len());
        let after = Projection::new(&vp, &data, 800.0, 400.0).index_at(300.0);
        assert!((before - after).abs() < 1e-6, "{before} vs {after}");
    }

    #[test]
    fn zoom_and_pan_stay_in_bounds() {
        let mut vp = Viewport::new(300);
        vp.zoom(1_000.0, 0.0, 800.0, 300);
        assert_eq!(vp.spacing, MAX_SPACING);
        vp.zoom(1e-6, 0.0, 800.0, 300);
        assert_eq!(vp.spacing, MIN_SPACING);

        vp.pan(1e9, 800.0, 300);
        assert_eq!(vp.right, 4.0, "can't scroll past the first bars");
        vp.pan(-1e9, 800.0, 300);
        assert!(
            vp.right <= 299.0 + 400.0,
            "can't scroll far into the future"
        );
    }

    #[test]
    fn price_zoom_expands_the_range() {
        let data = candles(200);
        let mut vp = Viewport::new(200);
        let fitted = Projection::new(&vp, &data, 800.0, 400.0);
        vp.scale_price(100.0);
        let zoomed = Projection::new(&vp, &data, 800.0, 400.0);
        assert!(zoomed.top - zoomed.bottom > fitted.top - fitted.bottom);
        vp.reset_price();
        assert_eq!(Projection::new(&vp, &data, 800.0, 400.0), fitted);
    }

    #[test]
    fn flat_data_still_has_height() {
        let flat = vec![Candle {
            ts: 0,
            open: 5.0,
            high: 5.0,
            low: 5.0,
            close: 5.0,
            volume: 0.0,
        }];
        let p = Projection::new(&Viewport::new(1), &flat, 800.0, 400.0);
        assert!(p.top > p.bottom);
        assert!(p.y(5.0).is_finite());
    }
}

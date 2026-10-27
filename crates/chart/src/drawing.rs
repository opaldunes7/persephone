//! Lines and boxes drawn on a chart. They're anchored to times and prices, so they stay put as
//! the chart pans and zooms, and show on every timeframe of the market.

use persephone_core::UnixNanos;
use serde::{Deserialize, Serialize};

/// A point on the chart: a moment and a price.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub ts: UnixNanos,
    pub price: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Drawing {
    TrendLine { from: Anchor, to: Anchor },
    HorizontalLine { price: f64 },
    Rectangle { from: Anchor, to: Anchor },
}

impl Drawing {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TrendLine { .. } => Tool::TrendLine.name(),
            Self::HorizontalLine { .. } => Tool::HorizontalLine.name(),
            Self::Rectangle { .. } => Tool::Rectangle.name(),
        }
    }
}

/// What the next clicks on a chart draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    TrendLine,
    HorizontalLine,
    Rectangle,
}

impl Tool {
    pub const ALL: [Self; 3] = [Self::TrendLine, Self::HorizontalLine, Self::Rectangle];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TrendLine => "Trend line",
            Self::HorizontalLine => "Horizontal line",
            Self::Rectangle => "Rectangle",
        }
    }

    /// How to place it, for menus.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::TrendLine | Self::Rectangle => "two clicks",
            Self::HorizontalLine => "one click",
        }
    }

    /// The drawing made from the clicks so far, if that's enough of them.
    pub(crate) fn complete(self, first: Option<Anchor>, latest: Anchor) -> Option<Drawing> {
        match (self, first) {
            (Self::HorizontalLine, _) => Some(Drawing::HorizontalLine {
                price: latest.price,
            }),
            (Self::TrendLine, Some(from)) => Some(Drawing::TrendLine { from, to: latest }),
            (Self::Rectangle, Some(from)) => Some(Drawing::Rectangle { from, to: latest }),
            (_, None) => None,
        }
    }
}

/// Distance from `p` to the segment `a`–`b`, in pixels.
pub(crate) fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0)
    };
    let (x, y) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - x).powi(2) + (p.1 - y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_need_their_clicks() {
        let a = Anchor { ts: 1, price: 10.0 };
        let b = Anchor { ts: 2, price: 20.0 };
        assert_eq!(
            Tool::HorizontalLine.complete(None, a),
            Some(Drawing::HorizontalLine { price: 10.0 })
        );
        assert_eq!(Tool::TrendLine.complete(None, a), None);
        assert_eq!(
            Tool::TrendLine.complete(Some(a), b),
            Some(Drawing::TrendLine { from: a, to: b })
        );
        assert_eq!(
            Tool::Rectangle.complete(Some(a), b).unwrap().name(),
            "Rectangle"
        );
    }

    #[test]
    fn distances_to_segments() {
        assert_eq!(
            distance_to_segment((5.0, 3.0), (0.0, 0.0), (10.0, 0.0)),
            3.0
        );
        assert_eq!(
            distance_to_segment((-4.0, 3.0), (0.0, 0.0), (10.0, 0.0)),
            5.0,
            "past the end"
        );
        assert_eq!(
            distance_to_segment((1.0, 1.0), (0.0, 0.0), (0.0, 0.0)),
            2f32.sqrt()
        );
    }
}

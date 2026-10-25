//! A backtest's trades on the chart: an arrow at every fill and a tint behind the bars while
//! a position is open.

use persephone_core::Side;

/// A fill, as far as the chart cares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillMark {
    /// Candle index the fill belongs to.
    pub bar: usize,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TradeMarker {
    pub bar: usize,
    pub side: Side,
    pub price: f64,
    /// The position after this fill: "Long", "Short" or "Close".
    pub label: &'static str,
}

/// Bars `from..=to` during which a position was open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionSpan {
    pub from: usize,
    pub to: usize,
    pub long: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trades {
    /// Sorted by bar.
    pub markers: Vec<TradeMarker>,
    pub spans: Vec<PositionSpan>,
}

impl Trades {
    /// Labels each fill by the position it leaves and works out the spans in between. A
    /// position still open at the end runs to the last of `bars` candles.
    #[must_use]
    pub fn from_fills(fills: &[FillMark], bars: usize) -> Self {
        let mut position = 0.0_f64;
        let mut open: Option<(usize, bool)> = None;
        let mut trades = Self::default();

        for fill in fills {
            position += if fill.side == Side::Buy {
                fill.qty
            } else {
                -fill.qty
            };
            // Quantities are rounded to the lot size, so treat dust as flat.
            let direction = if position > 1e-9 {
                1
            } else if position < -1e-9 {
                -1
            } else {
                0
            };
            trades.markers.push(TradeMarker {
                bar: fill.bar,
                side: fill.side,
                price: fill.price,
                label: match direction {
                    1 => "Long",
                    -1 => "Short",
                    _ => "Close",
                },
            });

            if let Some((from, long)) = open
                && (direction == 0 || (direction > 0) != long)
            {
                trades.spans.push(PositionSpan {
                    from,
                    to: fill.bar,
                    long,
                });
                open = None;
            }
            if direction != 0 && open.is_none() {
                open = Some((fill.bar, direction > 0));
            }
        }
        if let Some((from, long)) = open {
            trades.spans.push(PositionSpan {
                from,
                to: bars.saturating_sub(1).max(from),
                long,
            });
        }
        trades.markers.sort_by_key(|m| m.bar);
        trades
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(bar: usize, side: Side, qty: f64) -> FillMark {
        FillMark {
            bar,
            side,
            price: 100.0,
            qty,
        }
    }

    #[test]
    fn labels_follow_the_position_and_spans_cover_holding_periods() {
        let fills = [
            fill(10, Side::Buy, 1.0),  // long
            fill(20, Side::Sell, 2.0), // reverse to short
            fill(30, Side::Buy, 1.0),  // flat
            fill(40, Side::Buy, 0.5),  // long again, still open
        ];
        let trades = Trades::from_fills(&fills, 100);
        let labels: Vec<_> = trades.markers.iter().map(|m| m.label).collect();
        assert_eq!(labels, ["Long", "Short", "Close", "Long"]);
        assert_eq!(
            trades.spans,
            vec![
                PositionSpan {
                    from: 10,
                    to: 20,
                    long: true
                },
                PositionSpan {
                    from: 20,
                    to: 30,
                    long: false
                },
                PositionSpan {
                    from: 40,
                    to: 99,
                    long: true
                },
            ]
        );
    }

    #[test]
    fn no_fills_no_trades() {
        assert_eq!(Trades::from_fills(&[], 50), Trades::default());
    }
}

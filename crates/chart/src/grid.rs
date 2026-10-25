//! Bottom layer: background, grid lines and indicator reference levels. Cached until the
//! viewport, data or studies change.

use iced::{
    Point, Rectangle, Renderer, Size, Theme, mouse,
    widget::canvas::{self, LineDash, Path, Stroke},
};

use crate::{ChartState, plot_size, theme, ticks};

pub(crate) struct Grid<'a> {
    pub(crate) chart: &'a ChartState,
}

impl<Message> canvas::Program<Message> for Grid<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let geometry = self
            .chart
            .grid_cache
            .draw(renderer, bounds.size(), |frame| {
                frame.fill_rectangle(Point::ORIGIN, frame.size(), theme::BACKGROUND);

                let plot = plot_size(bounds.size());
                if plot.width < 1.0 || plot.height < 1.0 {
                    return;
                }
                let data = &self.chart.data;
                let panes = self.chart.panes(plot);

                if let Some(trades) = &self.chart.trades {
                    let price = &panes.price;
                    for span in &trades.spans {
                        let left = price.x(span.from as f64).max(0.0);
                        let right = price.x(span.to as f64).min(plot.width);
                        if right > left {
                            let tint = if span.long {
                                theme::LONG_SHADE
                            } else {
                                theme::SHORT_SHADE
                            };
                            frame.fill_rectangle(
                                Point::new(left, price.offset),
                                Size::new(right - left, price.height),
                                tint,
                            );
                        }
                    }
                }

                for pane in panes.all() {
                    for value in ticks::price_ticks(pane) {
                        let y = pane.y(value).round();
                        if pane.contains_y(y) {
                            frame.fill_rectangle(
                                Point::new(0.0, y),
                                Size::new(plot.width, 1.0),
                                theme::GRID,
                            );
                        }
                    }
                }
                for tick in ticks::time_ticks(&data.candles, &panes.price, data.timeframe) {
                    let x = panes.price.x(tick.index as f64).round();
                    let color = if tick.major {
                        theme::GRID_MAJOR
                    } else {
                        theme::GRID
                    };
                    frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, plot.height), color);
                }

                let level_stroke = Stroke {
                    line_dash: LineDash {
                        segments: &[3.0, 3.0],
                        offset: 0,
                    },
                    ..Stroke::default().with_color(theme::LEVEL).with_width(1.0)
                };
                for (study, pane) in &panes.indicators {
                    for &level in &self.chart.studies[*study].levels {
                        let y = pane.y(level).round() + 0.5;
                        if pane.contains_y(y) {
                            frame.stroke(
                                &Path::line(Point::new(0.0, y), Point::new(plot.width, y)),
                                level_stroke,
                            );
                        }
                    }
                }
            });
        vec![geometry]
    }
}

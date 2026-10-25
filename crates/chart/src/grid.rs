//! Bottom layer: background and grid lines. Cached until the viewport or data changes.

use iced::{Point, Rectangle, Renderer, Size, Theme, mouse, widget::canvas};

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
                let p = self.chart.projection(plot);

                for price in ticks::price_ticks(&p) {
                    let y = p.y(price).round();
                    frame.fill_rectangle(
                        Point::new(0.0, y),
                        Size::new(plot.width, 1.0),
                        theme::GRID,
                    );
                }
                for tick in ticks::time_ticks(&data.candles, &p, data.timeframe) {
                    let x = p.x(tick.index as f64).round();
                    let color = if tick.major {
                        theme::GRID_MAJOR
                    } else {
                        theme::GRID
                    };
                    frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, plot.height), color);
                }
            });
        vec![geometry]
    }
}

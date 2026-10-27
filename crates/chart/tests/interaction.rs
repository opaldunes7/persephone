//! Drives the chart with real mouse events through iced's headless simulator and checks the
//! messages it publishes.

use std::sync::Arc;

use iced::{Event, Point, Settings, mouse};
use iced_test::Simulator;
use persephone_chart::{ChartData, ChartMessage, ChartState, PRICE_AXIS_WIDTH, TIME_AXIS_HEIGHT};
use persephone_core::{Candle, Timeframe};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 600.0;
const PLOT_WIDTH: f32 = WIDTH - PRICE_AXIS_WIDTH;

fn data() -> Arc<ChartData> {
    let candles = (0..2_000u64)
        .map(|i| {
            let p = 100.0 + (i as f64 / 50.0).sin() * 10.0;
            Candle {
                ts: i * Timeframe::M1.nanos(),
                open: p,
                high: p + 1.0,
                low: p - 1.0,
                close: p + 0.5,
                volume: 3.0,
            }
        })
        .collect();
    ChartData::new("TESTUSDT", "SIM", Timeframe::M1, 2, candles)
}

/// Runs `steps` against a fresh chart and returns the published messages, leaving out the
/// hover reports (crosshair sync) that every mouse move over a new bar sends.
fn interact(steps: impl FnOnce(&mut Simulator<'_, ChartMessage>)) -> Vec<ChartMessage> {
    interact_all(steps)
        .into_iter()
        .filter(|m| !matches!(m, ChartMessage::Hovered { .. }))
        .collect()
}

fn interact_all(steps: impl FnOnce(&mut Simulator<'_, ChartMessage>)) -> Vec<ChartMessage> {
    let chart = ChartState::new(data());
    let mut ui = Simulator::with_size(Settings::default(), (WIDTH, HEIGHT), chart.view());
    steps(&mut ui);
    ui.into_messages().collect()
}

#[test]
fn hovering_reports_each_new_bar_once_and_leaving() {
    let messages = interact_all(|ui| {
        move_to(ui, 300.0, 200.0);
        move_to(ui, 300.5, 210.0); // the same bar
        move_to(ui, 340.0, 200.0);
        ui.simulate([Event::Mouse(mouse::Event::CursorLeft)]);
    });
    let hovers: Vec<Option<u64>> = messages
        .iter()
        .filter_map(|m| match m {
            ChartMessage::Hovered { ts } => Some(*ts),
            _ => None,
        })
        .collect();
    assert_eq!(hovers.len(), 3, "{hovers:?}");
    assert!(hovers[0].is_some() && hovers[1].is_some() && hovers[0] != hovers[1]);
    assert_eq!(hovers[2], None);
}

fn move_to(ui: &mut Simulator<'_, ChartMessage>, x: f32, y: f32) {
    ui.point_at(Point::new(x, y));
    ui.simulate([Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(x, y),
    })]);
}

fn press(ui: &mut Simulator<'_, ChartMessage>) {
    ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Left,
    ))]);
}

fn release(ui: &mut Simulator<'_, ChartMessage>) {
    ui.simulate([Event::Mouse(mouse::Event::ButtonReleased(
        mouse::Button::Left,
    ))]);
}

fn right_click(ui: &mut Simulator<'_, ChartMessage>) {
    ui.simulate([
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)),
    ]);
}

#[test]
fn dragging_the_plot_pans() {
    let messages = interact(|ui| {
        move_to(ui, 600.0, 300.0);
        press(ui);
        move_to(ui, 450.0, 320.0);
        move_to(ui, 400.0, 340.0);
        release(ui);
    });
    let pans: Vec<f32> = messages
        .iter()
        .map(|m| match m {
            ChartMessage::Pan { dx, plot_width } => {
                assert_eq!(*plot_width, PLOT_WIDTH);
                *dx
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(pans, vec![-150.0, -50.0]);
}

#[test]
fn scrolling_zooms_around_the_cursor_and_swiping_pans() {
    let messages = interact(|ui| {
        move_to(ui, 300.0, 200.0);
        ui.simulate([Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 50.0 },
        })]);
        ui.simulate([Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: -30.0, y: 4.0 },
        })]);
    });
    match &messages[..] {
        [
            ChartMessage::Zoom {
                factor, from_right, ..
            },
            ChartMessage::Pan { dx, .. },
        ] => {
            assert!(*factor > 1.0, "scrolling up zooms in");
            assert_eq!(*from_right, PLOT_WIDTH - 300.0);
            assert_eq!(*dx, -30.0);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn dragging_the_price_axis_scales_and_double_click_resets() {
    let axis_x = PLOT_WIDTH + PRICE_AXIS_WIDTH / 2.0;
    let messages = interact(|ui| {
        move_to(ui, axis_x, 200.0);
        press(ui);
        move_to(ui, axis_x, 260.0);
        release(ui);
        press(ui);
        release(ui);
    });
    match &messages[..] {
        [
            ChartMessage::ScalePrice { dy },
            ChartMessage::ResetPriceScale,
        ] => assert_eq!(*dy, 60.0),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn dragging_the_time_axis_scales_and_double_click_resets() {
    let axis_y = HEIGHT - TIME_AXIS_HEIGHT / 2.0;
    let messages = interact(|ui| {
        move_to(ui, 300.0, axis_y);
        press(ui);
        move_to(ui, 200.0, axis_y);
        release(ui);
        press(ui);
        release(ui);
    });
    match &messages[..] {
        [
            ChartMessage::ScaleTime { dx, .. },
            ChartMessage::ResetTimeScale,
        ] => assert_eq!(*dx, -100.0),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn messages_move_the_viewport() {
    let mut chart = ChartState::new(data());
    let start = *chart.viewport();

    chart.update(ChartMessage::Pan {
        dx: 80.0,
        plot_width: PLOT_WIDTH,
    });
    assert!(
        chart.viewport().right < start.right,
        "dragging right shows the past"
    );

    chart.update(ChartMessage::Zoom {
        factor: 2.0,
        from_right: 100.0,
        plot_width: PLOT_WIDTH,
    });
    assert_eq!(chart.viewport().spacing, start.spacing * 2.0);

    chart.update(ChartMessage::ScalePrice { dy: 40.0 });
    assert!(chart.viewport().price_zoom > 1.0);
    chart.update(ChartMessage::ResetPriceScale);
    assert_eq!(chart.viewport().price_zoom, 1.0);

    // The time axis: dragging left fits more bars, keeping the newest one on screen in place.
    let right = chart.viewport().right;
    chart.update(ChartMessage::ScaleTime {
        dx: -100.0,
        plot_width: PLOT_WIDTH,
    });
    assert!(chart.viewport().spacing < start.spacing * 2.0);
    assert_eq!(chart.viewport().right, right);
    chart.update(ChartMessage::ResetTimeScale);
    assert_eq!(chart.viewport().spacing, start.spacing);
}

mod clicks {
    use iced::{Point, Settings};
    use iced_test::Simulator;
    use persephone_chart::{
        ChartMessage, ChartState, OutputStyle, Placement, Study, StudyLine, theme,
    };

    use super::{HEIGHT, WIDTH, data, move_to, press, release, right_click};

    fn with_study(placement: Placement) -> ChartState {
        let mut chart = ChartState::new(data());
        let values = vec![50.0; chart.data().candles.len()];
        chart.set_studies(vec![Study {
            label: "RSI 14".into(),
            placement,
            lines: vec![StudyLine {
                name: "Value".into(),
                values,
                color: theme::STUDY_PALETTE[2],
                style: OutputStyle::Line,
            }],
            fill: None,
            levels: vec![30.0, 70.0],
            range: Some((0.0, 100.0)),
            error: None,
        }]);
        chart
    }

    fn clicks(
        chart: &ChartState,
        steps: impl FnOnce(&mut Simulator<'_, ChartMessage>),
    ) -> Vec<ChartMessage> {
        let mut ui = Simulator::with_size(Settings::default(), (WIDTH, HEIGHT), chart.view());
        steps(&mut ui);
        ui.into_messages()
            .filter(|m| !matches!(m, ChartMessage::Hovered { .. }))
            .collect()
    }

    #[test]
    fn left_and_right_clicks_are_told_apart() {
        let chart = ChartState::new(data());
        let messages = clicks(&chart, |ui| {
            move_to(ui, 400.0, 300.0);
            press(ui);
            move_to(ui, 402.0, 301.0); // jitter under the slop: not a pan
            release(ui);
            right_click(ui);
        });
        match &messages[..] {
            [
                ChartMessage::Clicked { at: left },
                ChartMessage::RightClicked { at: right },
            ] => {
                assert_eq!(*left, Point::new(400.0, 300.0));
                assert_eq!(*right, Point::new(402.0, 301.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_headers_symbol_and_timeframe_are_clickable() {
        // "TESTUSDT · 1m · SIM": the symbol, then the timeframe, then the venue (just a click).
        let chart = ChartState::new(data());
        let messages = clicks(&chart, |ui| {
            for x in [40.0, 105.0, 160.0] {
                move_to(ui, x, 16.0);
                press(ui);
                release(ui);
            }
        });
        match &messages[..] {
            [
                ChartMessage::SymbolClicked { .. },
                ChartMessage::TimeframeClicked { .. },
                ChartMessage::Clicked { .. },
            ] => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn clicking_a_pane_legend_selects_its_study() {
        let chart = with_study(Placement::Pane);
        // One indicator pane: the price pane is 452px tall (plot 574 - 122), the legend row
        // sits 14px into the pane below it.
        let messages = clicks(&chart, |ui| {
            move_to(ui, 30.0, 452.0 + 14.0);
            press(ui);
            release(ui);
        });
        match &messages[..] {
            [ChartMessage::StudyClicked { study: 0, .. }] => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn clicking_an_overlay_legend_selects_its_study() {
        let chart = with_study(Placement::Overlay);
        let messages = clicks(&chart, |ui| {
            move_to(ui, 30.0, 60.0);
            press(ui);
            release(ui);
        });
        assert!(
            matches!(&messages[..], [ChartMessage::StudyClicked { study: 0, .. }]),
            "{messages:?}"
        );
    }
}

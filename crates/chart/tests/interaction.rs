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

/// Runs `steps` against a fresh chart and returns the published messages.
fn interact(steps: impl FnOnce(&mut Simulator<'_, ChartMessage>)) -> Vec<ChartMessage> {
    let chart = ChartState::new(data());
    let mut ui = Simulator::with_size(Settings::default(), (WIDTH, HEIGHT), chart.view());
    steps(&mut ui);
    ui.into_messages().collect()
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
fn the_time_axis_ignores_drags() {
    let messages = interact(|ui| {
        move_to(ui, 300.0, HEIGHT - TIME_AXIS_HEIGHT / 2.0);
        press(ui);
        move_to(ui, 200.0, HEIGHT - TIME_AXIS_HEIGHT / 2.0);
        release(ui);
    });
    assert!(messages.is_empty(), "{messages:?}");
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
}

mod clicks {
    use iced::{Point, Settings};
    use iced_test::Simulator;
    use persephone_chart::{
        ChartMessage, ChartState, OutputStyle, Placement, Study, StudyLine, theme,
    };

    use super::{HEIGHT, WIDTH, data, move_to, press, release};

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
        ui.into_messages().collect()
    }

    #[test]
    fn a_still_click_opens_rather_than_pans() {
        let chart = ChartState::new(data());
        let messages = clicks(&chart, |ui| {
            move_to(ui, 400.0, 300.0);
            press(ui);
            move_to(ui, 402.0, 301.0); // jitter under the slop
            release(ui);
        });
        match &messages[..] {
            [ChartMessage::Clicked { at }] => assert_eq!(*at, Point::new(400.0, 300.0)),
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

//! Drives the whole app headlessly: key actions in, tile rectangles out.

use std::time::{Duration, Instant};

use iced::Size;
use persephone::{Message, Persephone, keymap::Action};
use persephone_chart::ChartData;
use persephone_core::{Candle, Timeframe};
use persephone_tiling::{Direction, MAX_TILES, Rect};

const WINDOW: Size = Size::new(1600.0, 900.0);

fn app() -> Persephone {
    let candles = (0..500u64)
        .map(|i| {
            let p = 100.0 + (i as f64 / 20.0).sin();
            Candle {
                ts: i * Timeframe::M1.nanos(),
                open: p,
                high: p + 0.5,
                low: p - 0.5,
                close: p + 0.2,
                volume: 1.0,
            }
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("TESTUSDT", "SIM", Timeframe::M1, 2, candles),
        WINDOW,
    )
}

fn press(app: &mut Persephone, action: Action) {
    let _ = app.update(Message::Key(action));
}

/// Jumps past every running animation.
fn settle(app: &mut Persephone) {
    let _ = app.update(Message::Tick(Instant::now() + Duration::from_secs(5)));
    assert!(!app.is_animating());
}

fn rect(app: &Persephone, id: u32) -> (f32, f32, f32, f32) {
    let r = app.tile_rect(id).expect("tile exists");
    (r.x, r.y, r.width, r.height)
}

#[test]
fn a_lone_chart_fills_the_window() {
    let app = app();
    assert_eq!(rect(&app, 0), (0.0, 0.0, 1600.0, 900.0));
}

#[test]
fn new_charts_dwindle_with_gaps_and_animate_in() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    assert!(app.is_animating(), "the new chart pops in");
    settle(&mut app);

    // 6px outer gap, 6px between: two 791px columns.
    assert_eq!(rect(&app, 0), (6.0, 6.0, 791.0, 888.0));
    assert_eq!(rect(&app, 1), (803.0, 6.0, 791.0, 888.0));
    assert_eq!(app.layout().focused(), 1);

    press(&mut app, Action::NewTile);
    settle(&mut app);
    assert_eq!(
        rect(&app, 2),
        (803.0, 453.0, 791.0, 441.0),
        "third goes under the second"
    );
}

#[test]
fn the_seventeenth_chart_is_refused_with_a_flash() {
    let mut app = app();
    for _ in 1..MAX_TILES {
        press(&mut app, Action::NewTile);
    }
    settle(&mut app);
    assert_eq!(app.layout().len(), MAX_TILES);

    press(&mut app, Action::NewTile);
    assert_eq!(app.layout().len(), MAX_TILES);
    assert!(app.is_animating(), "the focused border flashes");
}

#[test]
fn closing_hands_space_back_and_keeps_the_last_chart() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    press(&mut app, Action::CloseTile);
    settle(&mut app);
    assert_eq!(app.layout().len(), 1);
    assert_eq!(rect(&app, 0), (0.0, 0.0, 1600.0, 900.0));

    press(&mut app, Action::CloseTile);
    assert_eq!(app.layout().len(), 1, "the last chart stays");
    assert!(app.is_animating(), "and the refusal flashes");
}

#[test]
fn fullscreen_covers_the_window_and_returns() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    press(&mut app, Action::Fullscreen);
    settle(&mut app);
    assert_eq!(rect(&app, 1), (0.0, 0.0, 1600.0, 900.0));

    press(&mut app, Action::Fullscreen);
    settle(&mut app);
    assert_eq!(rect(&app, 1), (803.0, 6.0, 791.0, 888.0));
}

#[test]
fn keys_and_hover_move_focus() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    press(&mut app, Action::Focus(Direction::Left));
    assert_eq!(app.layout().focused(), 0);
    let _ = app.update(Message::Hover(1));
    assert_eq!(app.layout().focused(), 1, "focus follows the mouse");
}

#[test]
fn swap_resize_and_toggle_rearrange_tiles() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    settle(&mut app);

    press(&mut app, Action::Swap(Direction::Left));
    settle(&mut app);
    assert_eq!(rect(&app, 1).0, 6.0, "tile 1 moved left");

    press(&mut app, Action::Resize(Direction::Right));
    settle(&mut app);
    assert!(rect(&app, 1).2 > 791.0, "divider moved right, tile 1 grew");

    press(&mut app, Action::ToggleSplit);
    settle(&mut app);
    let Rect { width, .. } = app.tile_rect(0).unwrap();
    assert_eq!(width, 1588.0, "stacked: full width");
}

#[test]
fn window_resizes_snap_without_animating() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    settle(&mut app);
    let _ = app.update(Message::WindowResized(Size::new(1000.0, 700.0)));
    assert!(!app.is_animating());
    assert_eq!(rect(&app, 1), (503.0, 6.0, 491.0, 688.0));
}

mod restore {
    use std::path::PathBuf;

    use persephone::{Message, Persephone, keymap::Action, persist};
    use persephone_core::Timeframe;
    use persephone_tiling::Direction;

    fn temp_layout(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("persephone-restore-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("layout.ron")
    }

    #[test]
    fn layouts_are_saved_on_change_and_restored_at_launch() {
        let path = temp_layout("cycle");

        // First launch: nothing saved yet, so one default chart.
        let (mut app, _loads) = Persephone::start(&[], Some(path.clone()));
        assert_eq!(app.layout().len(), 1);
        assert_eq!(app.tile_market(0), Some(("BTCUSDT", Timeframe::M1.into())));

        let _ = app.update(Message::Key(Action::NewTile));
        let _ = app.update(Message::Key(Action::NewTile));
        let _ = app.update(Message::Key(Action::Resize(Direction::Left)));
        let saved = persist::load(&path).expect("saved after each change");
        assert_eq!(saved.layout, *app.layout());
        assert_eq!(saved.tiles.len(), 3);

        // Next launch picks up the same tiles, splits and focus.
        let (restored, _loads) = Persephone::start(&[], Some(path.clone()));
        assert_eq!(restored.layout(), app.layout());
        assert_eq!(
            restored.tile_market(2),
            Some(("BTCUSDT", Timeframe::M1.into()))
        );

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn arguments_start_a_one_off_chart() {
        let args = ["ethusdt".to_string(), "4h".to_string()];
        let (mut app, _loads) = Persephone::start(&args, None);
        assert_eq!(app.tile_market(0), Some(("ETHUSDT", Timeframe::H4.into())));
        let _ = app.update(Message::Key(Action::NewTile));
        assert_eq!(
            app.tile_market(1),
            Some(("ETHUSDT", Timeframe::H4.into())),
            "new charts copy the focused one"
        );
    }

    #[test]
    fn a_broken_file_starts_fresh() {
        let path = temp_layout("broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "(layout: oops)").unwrap();
        let (app, _loads) = Persephone::start(&[], Some(path.clone()));
        assert_eq!(app.layout().len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

#[test]
fn hovering_one_chart_moves_the_others_crosshair() {
    let mut app = app();
    press(&mut app, Action::NewTile);
    press(&mut app, Action::NewTile);
    let ts = 42 * Timeframe::M1.nanos();
    let _ = app.update(Message::Chart(
        0,
        persephone_chart::ChartMessage::Hovered { ts: Some(ts) },
    ));
    let linked = |app: &Persephone, id| app.tile_chart(id).unwrap().linked_time();
    assert_eq!(
        linked(&app, 0),
        None,
        "the hovered chart draws its own crosshair"
    );
    assert_eq!(linked(&app, 1), Some(ts));
    assert_eq!(linked(&app, 2), Some(ts));

    let _ = app.update(Message::Chart(
        0,
        persephone_chart::ChartMessage::Hovered { ts: None },
    ));
    assert_eq!(linked(&app, 1), None, "leaving the chart clears them");
}

mod workspaces {
    use std::sync::Arc;

    use persephone::{Message, Persephone, keymap::Action, menu::MenuMessage};
    use persephone_core::strategy::BuiltInStrategy;
    use persephone_engine::BacktestReport;

    use super::{app, press, rect, settle};

    #[test]
    fn each_workspace_keeps_its_own_charts() {
        let mut app = app();
        press(&mut app, Action::NewTile);
        settle(&mut app);
        let first = app.layout().tiles();
        let first_rects: Vec<_> = first.iter().map(|&id| rect(&app, id)).collect();

        press(&mut app, Action::Workspace(2));
        assert_eq!(app.workspace(), 2);
        assert_eq!(
            app.layout().len(),
            1,
            "a new workspace opens with one chart"
        );
        let second = app.layout().tiles()[0];
        assert!(
            !first.contains(&second),
            "ids never clash across workspaces"
        );
        assert!(
            app.tile_chart(second).is_some(),
            "it shows the same market straight away"
        );
        assert!(app.title().contains("workspace 2"), "{}", app.title());
        assert!(app.is_animating(), "the badge shows for a moment");

        press(&mut app, Action::Workspace(1));
        assert_eq!(app.layout().tiles(), first);
        let back: Vec<_> = first.iter().map(|&id| rect(&app, id)).collect();
        assert_eq!(back, first_rects);
        assert_eq!(app.workspaces(), vec![1, 2]);
        press(&mut app, Action::Workspace(1));
        assert_eq!(
            app.workspaces(),
            vec![1, 2],
            "switching to the current one does nothing"
        );
    }

    #[test]
    fn charts_move_between_workspaces_but_the_last_one_stays() {
        let mut app = app();
        press(&mut app, Action::MoveToWorkspace(3));
        assert_eq!(app.layout().len(), 1, "the only chart can't leave");
        assert_eq!(app.workspaces(), vec![1]);

        press(&mut app, Action::NewTile);
        press(&mut app, Action::NewTile);
        press(&mut app, Action::MoveToWorkspace(3));
        assert_eq!(app.layout().len(), 2);
        press(&mut app, Action::MoveToWorkspace(3));
        assert_eq!(app.layout().len(), 1);

        press(&mut app, Action::Workspace(3));
        assert_eq!(app.layout().len(), 2, "both arrived");
        assert!(
            app.layout()
                .tiles()
                .iter()
                .all(|&id| app.tile_chart(id).is_some())
        );
    }

    #[test]
    fn backtests_finishing_elsewhere_land_on_their_chart() {
        let mut app = app();
        let _ = app.update(Message::Chart(
            0,
            persephone_chart::ChartMessage::Clicked {
                at: iced::Point::new(50.0, 50.0),
            },
        ));
        let _ = app.update(Message::Menu(MenuMessage::SetStrategy(
            BuiltInStrategy::EmaCross,
        )));
        let (generation, _) = app.backtest_request(0).unwrap();

        press(&mut app, Action::Workspace(2));
        let report = Arc::new(BacktestReport::default());
        let _ = app.update(Message::BacktestDone(0, generation, Ok(report)));
        press(&mut app, Action::Workspace(1));
        assert!(matches!(
            app.tile_strategy(0).unwrap().1,
            persephone::Run::Done(_)
        ));
    }

    #[test]
    fn workspaces_are_saved_and_restored() {
        let dir =
            std::env::temp_dir().join(format!("persephone-workspaces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("layout.ron");
        let (mut app, _) = Persephone::start(&[], Some(path.clone()));
        press(&mut app, Action::NewTile);
        press(&mut app, Action::Workspace(4));
        press(&mut app, Action::NewTile);
        press(&mut app, Action::NewTile);

        let (restored, _) = Persephone::start(&[], Some(path));
        assert_eq!(restored.workspace(), 4);
        assert_eq!(restored.layout().len(), 3);
        assert_eq!(restored.workspaces(), vec![1, 4]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

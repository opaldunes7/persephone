//! The left-click menu and indicator settings, driven through the same messages the UI sends.

use std::path::PathBuf;

use iced::{Point, Size};
use persephone::{
    Message, Persephone,
    keymap::Action,
    menu::{MenuMessage, Page, Popup},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{
    Candle, Timeframe,
    indicator::{BuiltIn, IndicatorSpec},
};

fn app() -> Persephone {
    let candles = (0..400u64)
        .map(|i| {
            let p = 100.0 + (i as f64 / 15.0).sin() * 4.0;
            Candle {
                ts: i * Timeframe::M15.nanos(),
                open: p,
                high: p + 0.6,
                low: p - 0.6,
                close: p + 0.2,
                volume: 2.0,
            }
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("TESTUSDT", "SIM", Timeframe::M15, 2, candles),
        Size::new(1400.0, 900.0),
    )
}

fn click(app: &mut Persephone) {
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::Clicked {
            at: Point::new(300.0, 200.0),
        },
    ));
}

fn menu(app: &mut Persephone, message: MenuMessage) {
    let _ = app.update(Message::Menu(message));
}

fn add(app: &mut Persephone, kind: BuiltIn) {
    click(app);
    menu(app, MenuMessage::Open(Page::Indicators));
    menu(app, MenuMessage::AddIndicator(kind));
}

fn labels(app: &Persephone) -> Vec<String> {
    app.tile_chart(0)
        .unwrap()
        .studies()
        .iter()
        .map(|s| s.label.clone())
        .collect()
}

#[test]
fn a_click_opens_the_menu_and_another_closes_it() {
    let mut app = app();
    click(&mut app);
    assert!(matches!(
        app.popup(),
        Some(Popup::Menu {
            page: Page::Root,
            ..
        })
    ));
    click(&mut app);
    assert!(app.popup().is_none());

    click(&mut app);
    let _ = app.update(Message::Escape);
    assert!(app.popup().is_none(), "Escape closes it");

    click(&mut app);
    let _ = app.update(Message::Key(Action::NewTile));
    assert!(app.popup().is_none(), "so does any tiling key");
}

#[test]
fn indicators_are_added_drawn_and_listed() {
    let mut app = app();
    add(&mut app, BuiltIn::Ema);
    add(&mut app, BuiltIn::Rsi);
    add(&mut app, BuiltIn::Macd);

    assert!(app.popup().is_none(), "picking closes the menu");
    assert_eq!(
        app.tile_indicators(0).unwrap(),
        &[
            IndicatorSpec::new(BuiltIn::Ema),
            IndicatorSpec::new(BuiltIn::Rsi),
            IndicatorSpec::new(BuiltIn::Macd)
        ]
    );
    assert_eq!(labels(&app), ["EMA 21", "RSI 14", "MACD 12 26 9"]);
}

#[test]
fn settings_edit_and_remove_an_indicator() {
    let mut app = app();
    add(&mut app, BuiltIn::Ema);
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::StudyClicked {
            study: 0,
            at: Point::new(20.0, 60.0),
        },
    ));
    assert!(matches!(
        app.popup(),
        Some(Popup::Settings { study: 0, .. })
    ));

    menu(&mut app, MenuMessage::ParamChanged(0, "50".into()));
    assert_eq!(labels(&app), ["EMA 50"]);

    menu(&mut app, MenuMessage::ParamChanged(0, "5x".into()));
    assert!(
        matches!(app.popup(), Some(Popup::Settings { error: Some(_), .. })),
        "bad input is flagged"
    );
    assert_eq!(labels(&app), ["EMA 50"], "and ignored");

    menu(&mut app, MenuMessage::ParamChanged(0, "0".into()));
    assert_eq!(
        labels(&app),
        ["EMA 50"],
        "out-of-range values are ignored too"
    );

    menu(&mut app, MenuMessage::RemoveStudy);
    assert!(app.popup().is_none());
    assert!(labels(&app).is_empty());
}

#[test]
fn timeframe_and_symbol_switches_keep_indicators() {
    let mut app = app();
    add(&mut app, BuiltIn::Bollinger);

    click(&mut app);
    menu(&mut app, MenuMessage::SetTimeframe(Timeframe::H1));
    assert_eq!(app.tile_market(0), Some(("TESTUSDT", Timeframe::H1.into())));
    assert!(app.tile_chart(0).is_none(), "loading the new timeframe");
    assert_eq!(app.tile_indicators(0).unwrap().len(), 1);

    click(&mut app);
    menu(
        &mut app,
        MenuMessage::Open(Page::Symbol {
            query: String::new(),
        }),
    );
    menu(&mut app, MenuMessage::QueryChanged("solusdt".into()));
    menu(&mut app, MenuMessage::SubmitSymbol);
    assert_eq!(app.tile_market(0), Some(("SOLUSDT", Timeframe::H1.into())));
    assert!(app.popup().is_none());
}

#[test]
fn indicators_are_saved_and_restored() {
    let dir = std::env::temp_dir().join(format!("persephone-menu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path: PathBuf = dir.join("layout.ron");

    let (mut first, _loads) = Persephone::start(&[], Some(path.clone()));
    let _ = first.update(Message::Chart(
        0,
        ChartMessage::Clicked {
            at: Point::new(10.0, 10.0),
        },
    ));
    let _ = first.update(Message::Menu(MenuMessage::AddIndicator(
        BuiltIn::Stochastic,
    )));

    let (restored, _loads) = Persephone::start(&[], Some(path));
    assert_eq!(
        restored.tile_indicators(0).unwrap(),
        &[IndicatorSpec::new(BuiltIn::Stochastic)]
    );
    std::fs::remove_dir_all(dir).unwrap();
}

mod drawing {
    use iced::Point;
    use persephone::{
        Message, Persephone,
        keymap::Action,
        menu::{MenuMessage, Page, Popup},
    };
    use persephone_chart::{Anchor, ChartMessage, Drawing, Tool};
    use persephone_core::Timeframe;

    fn open_menu(app: &mut Persephone, id: u32) {
        let _ = app.update(Message::Chart(
            id,
            ChartMessage::Clicked {
                at: Point::new(80.0, 80.0),
            },
        ));
    }

    #[test]
    fn drawings_follow_their_market_and_can_be_removed() {
        let mut app = super::app();
        let _ = app.update(Message::Key(Action::NewTile));
        // The second chart: same market, another timeframe.
        open_menu(&mut app, 1);
        let _ = app.update(Message::Menu(MenuMessage::SetTimeframe(Timeframe::H1)));

        open_menu(&mut app, 0);
        let _ = app.update(Message::Menu(MenuMessage::Open(Page::Draw)));
        let _ = app.update(Message::Menu(MenuMessage::Draw(Tool::TrendLine)));
        assert!(app.popup().is_none(), "the menu closes to draw");
        assert_eq!(
            app.tile_chart(0).unwrap().drawing_tool(),
            Some(Tool::TrendLine)
        );

        let (a, b) = (
            Anchor { ts: 0, price: 1.0 },
            Anchor {
                ts: 3_600_000_000_000,
                price: 2.0,
            },
        );
        let _ = app.update(Message::Chart(0, ChartMessage::Placed(a)));
        let _ = app.update(Message::Chart(0, ChartMessage::Placed(b)));
        let line = Drawing::TrendLine { from: a, to: b };
        assert_eq!(app.tile_chart(0).unwrap().drawings(), &[line]);
        // The 1h chart may still be loading; once it has bars it gets the same drawings.

        let _ = app.update(Message::Chart(
            0,
            ChartMessage::DrawingClicked {
                index: 0,
                at: Point::new(10.0, 10.0),
            },
        ));
        assert!(matches!(app.popup(), Some(Popup::Drawing { index: 0, .. })));
        let _ = app.update(Message::Menu(MenuMessage::RemoveDrawing));
        assert!(app.tile_chart(0).unwrap().drawings().is_empty());
    }

    #[test]
    fn escape_stops_drawing() {
        let mut app = super::app();
        open_menu(&mut app, 0);
        let _ = app.update(Message::Menu(MenuMessage::Draw(Tool::HorizontalLine)));
        let _ = app.update(Message::Escape);
        assert_eq!(app.tile_chart(0).unwrap().drawing_tool(), None);
    }

    #[test]
    fn drawings_are_saved_next_to_the_layout() {
        let dir = std::env::temp_dir().join(format!("persephone-drawings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("layout.ron");
        let drawings = persephone::persist::Drawings::from([(
            "binance/BTCUSDT".to_owned(),
            vec![Drawing::HorizontalLine { price: 50_000.0 }],
        )]);
        persephone::persist::save_drawings(&dir.join("drawings.ron"), &drawings).unwrap();
        assert_eq!(
            persephone::persist::load_drawings(&dir.join("drawings.ron")),
            drawings
        );
        let (app, _) = Persephone::start(&[], Some(path));
        assert_eq!(app.layout().len(), 1, "starts fine with drawings saved");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

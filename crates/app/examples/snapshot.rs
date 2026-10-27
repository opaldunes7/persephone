//! Renders the whole app offscreen after a script of key actions.
//!
//! ```text
//! cargo run -p persephone --example snapshot -- OUT.png [SCRIPT] [AT_MS] [SYMBOL] [TIMEFRAME]
//! ```
//!
//! SCRIPT is one character per action: `n` new chart, `q` close, `h`/`j`/`k`/`l` focus,
//! `H`/`J`/`K`/`L` swap, `<`/`>`/`^`/`v` move a divider, `t` toggle split, `f` fullscreen;
//! `m` right-clicks the focused chart (opening its menu), `S` / `T` click the header's symbol
//! / timeframe, `E` types "ETH" into the symbol search, `A` unchecks the first item in the menu's Active list, `i` opens the indicator page, `s`
//! the symbol page, `1`-`8` add a built-in indicator (in menu order), `e` opens the first
//! indicator's settings; `x`/`y`/`z` set the EMA Cross / RSI Reversion / Donchian Breakout
//! strategy (menu open) and `r` opens the focused chart's strategy report. With the example
//! plugins built (`cargo build --release --target wasm32-wasip2` in `plugins/`), `a`/`b` add
//! the SMA / z-score plugin indicators, `w` sets the EMA Cross plugin strategy, `u` the LLM
//! Daily one (`U` allows it to ask models) and `P` opens the strategy page. Backtests run to completion before the snapshot.
//!
//! `R` paper trades the focused chart's strategy on a replay of its last 120 bars (as if they
//! came next), with the report open. `O` opens the sweep popup from the report and `Y` starts the sweep. `G` draws a trend line, a horizontal line and a box on the focused chart, and `c` hovers a
//! bar on it so the other charts show the synced crosshair.
//!
//! With `SNAPSHOT_STORE` naming a local tick store, `L` / `M` open its first / second market
//! (menu open), `D` drops the file named by `SNAPSHOT_DROP` on the focused chart (detecting its
//! layout here) and `I` imports it.
//! AT_MS freezes the clock that many milliseconds after the last action (default: settled),
//! to look at animation frames. iced appends the renderer name to the file name.

use std::{
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use iced::{Point, Settings, Size, Theme};
use persephone::{
    Message, Persephone, Run,
    keymap::Action,
    menu::{Active, ImportState, MenuMessage, Page, Popup},
};
use persephone_chart::{Anchor, ChartData, ChartMessage, Tool};
use persephone_core::{Candle, Timeframe, indicator::BuiltIn, strategy::BuiltInStrategy};
use persephone_data::{
    binance::BinanceClient,
    import::{self, MarketOptions, Overrides},
    store::Store,
};
use persephone_engine::live::LiveMode;
use persephone_plugin_host::{Host, Plugin, Plugins};
use persephone_tiling::Direction;

const WINDOW: Size = Size::new(1440.0, 900.0);

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .context("usage: snapshot OUT.png [SCRIPT] [AT_MS] [SYMBOL] [TIMEFRAME]")?;
    let script = args.next().unwrap_or_default();
    let at: Option<u64> = args
        .next()
        .filter(|a| a != "-")
        .map(|ms| ms.parse())
        .transpose()?;
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let timeframe: Timeframe = args.next().as_deref().unwrap_or("15m").parse()?;

    let mut app = Persephone::preloaded(fetch(&symbol, timeframe)?, WINDOW);
    let _ = app.set_plugins(example_plugins()?);
    let store = std::env::var_os("SNAPSHOT_STORE").map(Store::new);
    if let Some(store) = &store {
        app.set_store(store.clone());
    }
    for c in script.chars() {
        let focused = app.layout().focused();
        match c {
            'D' => drop_file(&mut app)?,
            'G' => draw_examples(&mut app, focused),
            'R' => rehearse_live(&mut app, focused)?,
            'c' => {
                let chart = app.tile_chart(focused).context("the chart has no bars")?;
                let candles = &chart.data().candles;
                let ts = candles[candles.len().saturating_sub(60)].ts;
                let _ = app.update(Message::Chart(
                    focused,
                    ChartMessage::Hovered { ts: Some(ts) },
                ));
            }
            'I' => import_dropped(&mut app, store.as_ref().context("set SNAPSHOT_STORE")?)?,
            'L' | 'M' => {
                let markets = store.as_ref().context("set SNAPSHOT_STORE")?.markets();
                let market = markets
                    .get(usize::from(c == 'M'))
                    .context("the store has too few markets")?;
                let _ = app.update(Message::Menu(MenuMessage::SetLocal {
                    venue: market.info.venue.clone(),
                    symbol: market.info.symbol.clone(),
                }));
            }
            c => {
                let _ = app.update(message(c, focused)?);
            }
        }
        app.load_now();
        finish_backtests(&mut app);
    }
    let frozen = Instant::now() + at.map_or(Duration::from_secs(5), Duration::from_millis);
    let _ = app.update(Message::Tick(frozen));

    let mut ui = iced_test::Simulator::with_size(Settings::default(), WINDOW, app.view());
    let snapshot = ui.snapshot(&Theme::Dark)?;
    remove_previous(Path::new(&out))?;
    snapshot.matches_image(&out)?;
    println!("wrote {} tiles next to {out}", app.layout().len());
    Ok(())
}

fn message(c: char, focused: u32) -> anyhow::Result<Message> {
    use Direction::{Down, Left, Right, Up};
    let key = |action| Ok(Message::Key(action));
    let menu = |message| Ok(Message::Menu(message));
    match c {
        'n' => key(Action::NewTile),
        'q' => key(Action::CloseTile),
        'h' => key(Action::Focus(Left)),
        'j' => key(Action::Focus(Down)),
        'k' => key(Action::Focus(Up)),
        'l' => key(Action::Focus(Right)),
        'H' => key(Action::Swap(Left)),
        'J' => key(Action::Swap(Down)),
        'K' => key(Action::Swap(Up)),
        'L' => key(Action::Swap(Right)),
        '<' => key(Action::Resize(Left)),
        '>' => key(Action::Resize(Right)),
        '^' => key(Action::Resize(Up)),
        'v' => key(Action::Resize(Down)),
        't' => key(Action::ToggleSplit),
        'f' => key(Action::Fullscreen),
        'm' => Ok(Message::Chart(
            focused,
            ChartMessage::RightClicked {
                at: Point::new(260.0, 160.0),
            },
        )),
        'e' => Ok(Message::Chart(
            focused,
            ChartMessage::StudyClicked {
                study: 0,
                at: Point::new(24.0, 60.0),
            },
        )),
        'i' => menu(MenuMessage::Open(Page::Indicators)),
        'P' => menu(MenuMessage::Open(Page::Strategies)),
        's' => menu(MenuMessage::Open(Page::Symbol {
            query: String::new(),
        })),
        '1'..='8' => menu(MenuMessage::AddIndicator(
            BuiltIn::ALL[c as usize - '1' as usize],
        )),
        'x' => menu(MenuMessage::SetStrategy(BuiltInStrategy::EmaCross)),
        'y' => menu(MenuMessage::SetStrategy(BuiltInStrategy::RsiReversion)),
        'z' => menu(MenuMessage::SetStrategy(BuiltInStrategy::DonchianBreakout)),
        'r' => Ok(Message::OpenReport(focused)),
        'S' => Ok(Message::Chart(
            focused,
            ChartMessage::SymbolClicked {
                at: Point::new(40.0, 16.0),
            },
        )),
        'T' => Ok(Message::Chart(
            focused,
            ChartMessage::TimeframeClicked {
                at: Point::new(100.0, 16.0),
            },
        )),
        'A' => menu(MenuMessage::Show(Active::Indicator(0), false)),
        'E' => menu(MenuMessage::QueryChanged("ETH".into())),
        'O' => menu(MenuMessage::OpenSweep),
        'Y' => menu(MenuMessage::StartSweep),
        'a' => menu(MenuMessage::AddPluginIndicator("sma".into())),
        'b' => menu(MenuMessage::AddPluginIndicator("zscore".into())),
        'w' => menu(MenuMessage::SetPluginStrategy("ema_cross".into())),
        'u' => menu(MenuMessage::SetPluginStrategy("llm_daily".into())),
        'U' => menu(MenuMessage::AllowAi(true)),
        other => bail!("unknown script action `{other}`"),
    }
}

/// A trend line under the last stretch of lows, a line at the latest close, and a box around a
/// range, on chart `id`.
fn draw_examples(app: &mut Persephone, id: u32) {
    let Some(chart) = app.tile_chart(id) else {
        return;
    };
    let candles = chart.data().candles.clone();
    let n = candles.len();
    if n < 200 {
        return;
    }
    let at = |i: usize, price: f64| Anchor {
        ts: candles[i].ts,
        price,
    };
    let (box_low, box_high) = candles[n - 110..n - 70]
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), c| {
            (lo.min(c.low), hi.max(c.high))
        });
    let drawings = [
        (
            Tool::TrendLine,
            vec![
                at(n - 180, candles[n - 180].low),
                at(n - 20, candles[n - 20].low),
            ],
        ),
        (Tool::HorizontalLine, vec![at(n - 1, candles[n - 1].close)]),
        (
            Tool::Rectangle,
            vec![at(n - 110, box_high), at(n - 70, box_low)],
        ),
    ];
    for (tool, anchors) in drawings {
        let _ = app.update(Message::Chart(
            id,
            ChartMessage::RightClicked {
                at: Point::new(5.0, 5.0),
            },
        ));
        let _ = app.update(Message::Menu(MenuMessage::Draw(tool)));
        for anchor in anchors {
            let _ = app.update(Message::Chart(id, ChartMessage::Placed(anchor)));
        }
    }
}

/// Drops `SNAPSHOT_DROP` on the focused chart and works out its layout, as the app's tasks would.
fn drop_file(app: &mut Persephone) -> anyhow::Result<()> {
    let path: std::path::PathBuf = std::env::var_os("SNAPSHOT_DROP")
        .context("set SNAPSHOT_DROP to a CSV file")?
        .into();
    let _ = app.update(Message::FileDropped(path.clone()));
    let detection = import::detect(&import::sample(&path)?, &Overrides::default())
        .map(std::sync::Arc::new)
        .map_err(|e| format!("{e:#}"));
    let _ = app.update(Message::Detected(path, detection));
    Ok(())
}

/// Presses Import on the import panel and runs the import here.
fn import_dropped(app: &mut Persephone, store: &Store) -> anyhow::Result<()> {
    let Some(Popup::Import {
        files,
        state: ImportState::Ready(detection),
        symbol,
        venue,
        ..
    }) = app.popup()
    else {
        bail!("`I` needs the import panel ready (drop a file with `D` first)");
    };
    let (files, format) = (files.clone(), detection.format.clone());
    let options = MarketOptions {
        symbol: symbol.clone(),
        venue: venue.clone(),
        base: None,
        quote: None,
        fee: None,
    };
    let _ = app.update(Message::Menu(MenuMessage::StartImport));
    let summary = import::import(store, &files, &format, &options, &mut |_| true)
        .map(std::sync::Arc::new)
        .map_err(|e| format!("{e:#}"));
    let _ = app.update(Message::Imported(summary));
    Ok(())
}

/// Runs every backtest the app is waiting on, here instead of on a worker thread.
/// Paper trades tile `id`'s strategy on its last 120 bars replayed as the next ones, until all
/// are in. The session keeps running; the snapshot shows it mid-way.
fn rehearse_live(app: &mut Persephone, id: u32) -> anyhow::Result<()> {
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task::into_stream};

    let candles = app
        .tile_chart(id)
        .context("the chart has no bars")?
        .data()
        .candles
        .clone();
    let n = candles.len();
    anyhow::ensure!(n > 130, "too few bars to replay");
    let step = candles[n - 1].ts - candles[n - 2].ts;
    let offset = candles[n - 1].open - candles[n - 121].open;
    let replay: Vec<Candle> = candles[n - 121..n - 1]
        .iter()
        .enumerate()
        .map(|(i, c)| Candle {
            ts: candles[n - 1].ts + i as u64 * step,
            open: c.open + offset,
            high: c.high + offset,
            low: c.low + offset,
            close: c.close + offset,
            volume: c.volume,
        })
        .collect();
    let wanted = n - 1 + replay.len();
    app.rehearse_live(replay, Duration::from_millis(2));
    let _ = app.update(Message::OpenReport(id));
    let task = app.update(Message::Menu(MenuMessage::GoLive(LiveMode::Paper)));
    let mut events = into_stream(task).context("no live session started")?;
    while let Some(action) = block_on(events.next()) {
        if let Action::Output(message) = action {
            let _ = app.update(message);
        }
        if app
            .tile_chart(id)
            .is_some_and(|c| c.data().candles.len() >= wanted)
        {
            break;
        }
    }
    // Bring the backtest up to the replayed bars too.
    if let Some((generation, request)) = app.backtest_request(id) {
        let result = persephone_engine::run_backtest(&request)
            .map(std::sync::Arc::new)
            .map_err(|e| e.to_string());
        let _ = app.update(Message::BacktestDone(id, generation, result));
    }
    Ok(())
}

fn finish_backtests(app: &mut Persephone) {
    for id in app.layout().tiles() {
        if !matches!(app.tile_strategy(id), Some((_, Run::Running))) {
            continue;
        }
        if let Some((generation, request)) = app.backtest_request(id) {
            let result = persephone_engine::run_backtest(&request)
                .map(std::sync::Arc::new)
                .map_err(|e| e.to_string());
            let _ = app.update(Message::BacktestDone(id, generation, result));
        }
    }
}

/// The example plugins from `plugins/`, if they've been built (the fixtures stay out).
fn example_plugins() -> anyhow::Result<Plugins> {
    let built =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target/wasm32-wasip2/release");
    let host = Host::new().map_err(anyhow::Error::msg)?;
    let mut plugins = Plugins::default();
    for name in ["sma", "zscore", "ema_cross", "llm_daily", "ml_trend"] {
        match host.load(&built.join(format!("{name}.wasm"))) {
            Ok(Plugin::Indicator(p)) => {
                plugins.indicators.insert(p.id.clone(), p);
            }
            Ok(Plugin::Strategy(p)) => {
                plugins.strategies.insert(p.id.clone(), p);
            }
            Err(error) => plugins.errors.push((format!("{name}.wasm"), error)),
        }
    }
    Ok(plugins)
}

fn fetch(symbol: &str, timeframe: Timeframe) -> anyhow::Result<std::sync::Arc<ChartData>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % timeframe.nanos() + timeframe.nanos();
    let binance = BinanceClient::new()?;
    let spec = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, end - 2_000 * timeframe.nanos(), end)?;
    Ok(ChartData::new(
        spec.symbol,
        spec.venue,
        timeframe,
        spec.price_precision,
        candles,
    ))
}

/// `matches_image` only writes when the file is missing, so clear old snapshots first.
fn remove_previous(out: &Path) -> anyhow::Result<()> {
    let stem = out
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let dir = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with(&format!("{stem}-")) && name.ends_with(".png") {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

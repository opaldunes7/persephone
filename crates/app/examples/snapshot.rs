//! Renders the whole app offscreen after a script of key actions.
//!
//! ```text
//! cargo run -p persephone --example snapshot -- OUT.png [SCRIPT] [AT_MS] [SYMBOL] [TIMEFRAME]
//! ```
//!
//! SCRIPT is one character per action: `n` new chart, `q` close, `h`/`j`/`k`/`l` focus,
//! `H`/`J`/`K`/`L` swap, `<`/`>`/`^`/`v` move a divider, `t` toggle split, `f` fullscreen;
//! `m` left-clicks the focused chart (opening its menu), `i` opens the indicator page, `s`
//! the symbol page, `1`-`8` add a built-in indicator (in menu order), `e` opens the first
//! indicator's settings; `x`/`y`/`z` set the EMA Cross / RSI Reversion / Donchian Breakout
//! strategy (menu open) and `r` opens the focused chart's strategy report. With the example
//! plugins built (`cargo build --release --target wasm32-wasip2` in `plugins/`), `a`/`b` add
//! the SMA / z-score plugin indicators, `w` sets the EMA Cross plugin strategy and `P` opens
//! the strategy page. Backtests run to completion before the snapshot.
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
    menu::{MenuMessage, Page},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{Timeframe, indicator::BuiltIn, strategy::BuiltInStrategy};
use persephone_data::binance::BinanceClient;
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
    for c in script.chars() {
        let focused = app.layout().focused();
        let _ = app.update(message(c, focused)?);
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
            ChartMessage::Clicked {
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
        'a' => menu(MenuMessage::AddPluginIndicator("sma".into())),
        'b' => menu(MenuMessage::AddPluginIndicator("zscore".into())),
        'w' => menu(MenuMessage::SetPluginStrategy("ema_cross".into())),
        other => bail!("unknown script action `{other}`"),
    }
}

/// Runs every backtest the app is waiting on, here instead of on a worker thread.
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
    for name in ["sma", "zscore", "ema_cross"] {
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

//! Renders the whole app offscreen after a script of key actions.
//!
//! ```text
//! cargo run -p persephone --example snapshot -- OUT.png [SCRIPT] [AT_MS] [SYMBOL] [TIMEFRAME]
//! ```
//!
//! SCRIPT is one character per action: `n` new chart, `q` close, `h`/`j`/`k`/`l` focus,
//! `H`/`J`/`K`/`L` swap, `<`/`>`/`^`/`v` move a divider, `t` toggle split, `f` fullscreen.
//! AT_MS freezes the clock that many milliseconds after the last action (default: settled),
//! to look at animation frames. iced appends the renderer name to the file name.

use std::{
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use iced::{Settings, Size, Theme};
use persephone::{Message, Persephone, keymap::Action};
use persephone_chart::ChartData;
use persephone_core::Timeframe;
use persephone_data::binance::BinanceClient;
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
    for c in script.chars() {
        let _ = app.update(Message::Key(action(c)?));
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

fn action(c: char) -> anyhow::Result<Action> {
    use Direction::{Down, Left, Right, Up};
    Ok(match c {
        'n' => Action::NewTile,
        'q' => Action::CloseTile,
        'h' => Action::Focus(Left),
        'j' => Action::Focus(Down),
        'k' => Action::Focus(Up),
        'l' => Action::Focus(Right),
        'H' => Action::Swap(Left),
        'J' => Action::Swap(Down),
        'K' => Action::Swap(Up),
        'L' => Action::Swap(Right),
        '<' => Action::Resize(Left),
        '>' => Action::Resize(Right),
        '^' => Action::Resize(Up),
        'v' => Action::Resize(Down),
        't' => Action::ToggleSplit,
        'f' => Action::Fullscreen,
        other => bail!("unknown script action `{other}`"),
    })
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

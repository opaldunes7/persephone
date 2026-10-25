//! Renders one chart offscreen to a PNG, through the same wgpu renderer the app uses.
//!
//! ```text
//! cargo run -p persephone-chart --example snapshot -- OUT.png [SYMBOL|synthetic] [TIMEFRAME] [X,Y|-] [ZOOM]
//! ```
//!
//! `X,Y` places the mouse (logical pixels) to show the crosshair; `-` leaves it off. `ZOOM`
//! multiplies the bar spacing (below 1 zooms out). iced appends the renderer
//! name to the file name, e.g. `OUT-wgpu.png`.

use std::{
    f64::consts::TAU,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use iced::{Point, Settings, Theme};
use persephone_chart::{ChartData, ChartState};
use persephone_core::{Candle, Timeframe};
use persephone_data::binance::BinanceClient;

const BARS: u64 = 5_000;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .context("usage: snapshot OUT.png [SYMBOL] [TIMEFRAME] [X,Y|-] [ZOOM]")?;
    let symbol = args.next().unwrap_or_else(|| "BTCUSDT".into());
    let timeframe: Timeframe = args.next().as_deref().unwrap_or("1m").parse()?;
    let cursor = args
        .next()
        .filter(|xy| xy != "-")
        .map(|xy| -> anyhow::Result<Point> {
            let (x, y) = xy.split_once(',').context("cursor must be X,Y")?;
            Ok(Point::new(x.parse()?, y.parse()?))
        })
        .transpose()?;
    let zoom: Option<f32> = args.next().map(|z| z.parse()).transpose()?;

    let data = if symbol == "synthetic" {
        synthetic(timeframe)
    } else {
        fetch(&symbol, timeframe)?
    };
    let mut chart = ChartState::new(data);
    if let Some(factor) = zoom {
        let plot_width = 1280.0 - persephone_chart::PRICE_AXIS_WIDTH;
        chart.update(persephone_chart::ChartMessage::Zoom {
            factor,
            from_right: 0.0,
            plot_width,
        });
    }

    let mut ui =
        iced_test::Simulator::with_size(Settings::default(), (1280.0, 800.0), chart.view());
    if let Some(point) = cursor {
        ui.point_at(point);
    }
    let snapshot = ui.snapshot(&Theme::Dark)?;

    remove_previous(Path::new(&out))?;
    snapshot.matches_image(&out)?;
    println!(
        "wrote {} snapshot next to {out}",
        if cursor.is_some() {
            "crosshair"
        } else {
            "plain"
        }
    );
    Ok(())
}

fn fetch(symbol: &str, timeframe: Timeframe) -> anyhow::Result<std::sync::Arc<ChartData>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % timeframe.nanos() + timeframe.nanos();
    let binance = BinanceClient::new()?;
    let spec = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, end - BARS * timeframe.nanos(), end)?;
    Ok(ChartData::new(
        spec.symbol,
        spec.venue,
        timeframe,
        spec.price_precision,
        candles,
    ))
}

/// A deterministic random-ish walk, for snapshots without network access.
fn synthetic(timeframe: Timeframe) -> std::sync::Arc<ChartData> {
    let start = 1_792_454_400_000_000_000; // 2026-10-20 00:00 UTC
    let mut close = 80_000.0;
    let candles = (0..BARS)
        .map(|i| {
            let t = i as f64;
            let drift = 120.0 * (t / 300.0 * TAU).sin() + 40.0 * (t / 37.0).sin();
            let open = close;
            close = (open + drift * 0.05 + 25.0 * (t * 12.9898).sin()).max(1.0);
            let wick = 15.0 + 20.0 * (t * 4.1414).sin().abs();
            Candle {
                ts: start + i * timeframe.nanos(),
                open,
                high: open.max(close) + wick,
                low: open.min(close) - wick,
                close,
                volume: 5.0 + 40.0 * (t * 7.77).sin().abs(),
            }
        })
        .collect();
    ChartData::new("SYNTHUSDT", "SIM", timeframe, 2, candles)
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

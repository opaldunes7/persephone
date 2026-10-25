//! `persephone-cli`: headless tools for Persephone (backtests now; fetch, import and plugin
//! scaffolding as later milestones land).

use std::{
    collections::BTreeMap,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use persephone_core::{Side, Timeframe, UnixNanos};
use persephone_data::binance::BinanceClient;
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates, StrategySpec, spawn_backtest};

const NANOS_PER_DAY: u64 = 86_400_000_000_000;

#[derive(Parser)]
#[command(name = "persephone-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch Binance spot bars and run the built-in EMA-cross strategy on them.
    Backtest(BacktestArgs),
}

#[derive(clap::Args)]
struct BacktestArgs {
    /// Binance spot symbol.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Bar timeframe: 1m, 5m, 15m, 30m, 1h, 4h or 1d.
    #[arg(long, default_value = "1h")]
    timeframe: Timeframe,
    /// How many days of history to test, ending now.
    #[arg(long, default_value_t = 90)]
    days: u64,
    /// Fast EMA period.
    #[arg(long, default_value_t = 10)]
    fast: usize,
    /// Slow EMA period.
    #[arg(long, default_value_t = 30)]
    slow: usize,
    /// Position size in the base asset (e.g. BTC).
    #[arg(long, default_value_t = 0.1)]
    size: f64,
    /// Starting balance in the quote asset (e.g. USDT).
    #[arg(long, default_value_t = 100_000.0)]
    balance: f64,
    /// Fee schedule.
    #[arg(long, value_enum, default_value_t = Fees::Binance)]
    fees: Fees,
    /// Print every fill.
    #[arg(long)]
    fills: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Fees {
    /// Binance spot base tier, 0.1% maker and taker.
    Binance,
    Zero,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Backtest(args) => backtest(&args),
    }
}

fn backtest(args: &BacktestArgs) -> anyhow::Result<()> {
    let tf = args.timeframe;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % tf.nanos();
    let start = end.saturating_sub(args.days * NANOS_PER_DAY);

    let fetch_started = Instant::now();
    let binance = BinanceClient::new()?;
    let instrument = binance.instrument(&args.symbol)?;
    let candles = binance
        .klines(&args.symbol, tf, start, end)
        .context("fetching bars from Binance")?;
    let fetch_secs = fetch_started.elapsed().as_secs_f64();
    anyhow::ensure!(
        !candles.is_empty(),
        "Binance returned no bars for {}",
        args.symbol
    );

    println!(
        "{} {tf} · {} → {} · {} bars · EMA {}/{} · size {} {}",
        instrument.symbol,
        date(candles[0].ts),
        date(candles[candles.len() - 1].ts),
        candles.len(),
        args.fast,
        args.slow,
        args.size,
        instrument.base,
    );

    let quote = instrument.quote.clone();
    let request = BacktestRequest {
        instrument,
        timeframe: tf,
        candles,
        strategy: StrategySpec::EmaCross {
            fast: args.fast,
            slow: args.slow,
            trade_size: args.size,
        },
        starting_balance: args.balance,
        fees: match args.fees {
            Fees::Binance => FeeRates::BINANCE_SPOT,
            Fees::Zero => FeeRates::ZERO,
        },
    };

    let run_started = Instant::now();
    let report = spawn_backtest(request)
        .join()
        .map_err(|_| anyhow::anyhow!("backtest thread panicked"))??;
    println!(
        "fetched in {fetch_secs:.1}s, backtested in {:.2}s\n",
        run_started.elapsed().as_secs_f64()
    );

    print_summary(&report, &quote);
    if args.fills {
        print_fills(&report);
    }
    Ok(())
}

fn print_summary(report: &BacktestReport, quote: &str) {
    let pnl = |key: &str| report.pnl.get(key).copied().unwrap_or(f64::NAN);
    println!(
        "  {:<14}{} fills · {} positions",
        "Trades",
        report.fills.len(),
        report.total_positions
    );
    println!(
        "  {:<14}{:+.2} {quote} ({:+.2}%)",
        "PnL",
        pnl("PnL (total)"),
        pnl("PnL% (total)")
    );
    println!("  {:<14}{}", "Win rate", percent(pnl("Win Rate")));
    println!();

    print_section("PnL", &report.pnl);
    print_section("Returns", &report.returns);
    print_section("General", &report.general);
}

fn print_section(title: &str, stats: &BTreeMap<String, f64>) {
    if stats.is_empty() {
        return;
    }
    println!("{title}");
    for (name, value) in stats {
        println!("  {name:<32}{}", number(*value));
    }
    println!();
}

fn print_fills(report: &BacktestReport) {
    println!("Fills");
    for fill in &report.fills {
        let side = match fill.side {
            Side::Buy => "BUY ",
            Side::Sell => "SELL",
        };
        println!(
            "  {}  {side}  {:>12.2} × {}",
            datetime(fill.ts),
            fill.price,
            fill.qty
        );
    }
}

fn number(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "—".into()
    }
}

fn percent(v: f64) -> String {
    if v.is_finite() {
        format!("{:.1}%", v * 100.0)
    } else {
        "—".into()
    }
}

/// `YYYY-MM-DD` for a UTC timestamp.
fn date(ts: UnixNanos) -> String {
    let (y, m, d) = civil_from_days((ts / NANOS_PER_DAY) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD HH:MM` for a UTC timestamp.
fn datetime(ts: UnixNanos) -> String {
    let minutes = (ts % NANOS_PER_DAY) / 60_000_000_000;
    format!("{} {:02}:{:02}", date(ts), minutes / 60, minutes % 60)
}

/// Days since 1970-01-01 → (year, month, day), from Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(datetime(1_735_689_600_000_000_000), "2025-01-01 00:00");
        assert_eq!(datetime(1_792_947_600_000_000_000), "2026-10-25 17:00");
        assert_eq!(date(951_782_400_000_000_000), "2000-02-29");
    }
}

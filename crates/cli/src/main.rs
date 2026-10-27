//! `persephone-cli`: headless tools for Persephone: backtests, paper and live trading,
//! importing CSV tick data, scaffolding, building and installing plugins, and setting up their
//! models and AI access.

mod data;
mod live;
mod ml;
mod plugin;

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Side, UnixNanos,
    signal::SignalFactory,
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
    tick::{self, TickFeed},
    time::{DateTime, NANOS_PER_DAY},
};
use persephone_data::{binance::BinanceClient, store::Store};
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates, spawn_backtest};
use persephone_plugin_host::{Host, external};

#[derive(Parser)]
#[command(name = "persephone-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Backtest a strategy on Binance spot bars, or on a market from the local store.
    Backtest(BacktestArgs),
    /// Trade a strategy on Binance bars as they close: on paper (the default), on
    /// Binance's testnet, or with real money.
    Live(live::LiveArgs),
    /// Import CSV tick files (trades or quotes) into the local store.
    Import(Box<data::ImportArgs>),
    /// List or remove markets in the local store.
    #[command(subcommand)]
    Data(data::DataCommand),
    /// Download market data archives and import them into the local store.
    #[command(subcommand)]
    Fetch(data::FetchSource),
    /// Create, build and install WebAssembly plugins.
    #[command(subcommand)]
    Plugin(plugin::PluginCommand),
    /// The folder of ONNX models plugins can run.
    #[command(subcommand)]
    Models(ml::ModelsCommand),
    /// How plugins reach language models, and their recorded replies.
    #[command(subcommand)]
    Ai(ml::AiCommand),
}

#[derive(clap::Args)]
struct BacktestArgs {
    /// Binance spot symbol.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Test a market from the local store instead (SYMBOL.VENUE, as `data list` shows it).
    /// Orders fill on its ticks.
    #[arg(long, conflicts_with = "symbol")]
    local: Option<String>,
    /// With --local, fill orders on the bars instead of the ticks.
    #[arg(long, requires = "local")]
    bar_fills: bool,
    /// Bars: a timeframe (1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w, 1M), or for --local markets with
    /// ticks a bar every so many ticks (1000t) or so much traded size (2.5v).
    #[arg(long, alias = "bars", default_value = "1h")]
    timeframe: BarSpec,
    /// How many days of history to test, ending now (or at the local market's last tick).
    #[arg(long, default_value_t = 90)]
    days: u64,
    /// Strategy: ema-cross, rsi-reversion or donchian-breakout.
    #[arg(long, default_value = "ema-cross", value_parser = parse_strategy)]
    strategy: BuiltInStrategy,
    /// Strategy parameters in order, comma-separated (e.g. `10,30,10` for EMA Cross fast,
    /// slow, size %). Missing ones keep their defaults.
    #[arg(long, value_delimiter = ',')]
    params: Vec<f64>,
    /// Run a strategy in a process of its own instead, listening at this address
    /// (tcp://HOST:PORT or unix://PATH); see plugins/external.
    #[arg(long)]
    external: Option<String>,
    /// Starting balance in the quote asset (e.g. USDT).
    #[arg(long, default_value_t = 100_000.0)]
    balance: f64,
    /// Fee schedule (default: Binance's for Binance, the market's own for --local).
    #[arg(long, value_enum)]
    fees: Option<Fees>,
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

/// The strategy to run: a built-in, or the one listening at `external`, with `params`.
fn strategy(
    kind: BuiltInStrategy,
    params: &[f64],
    external: Option<&str>,
) -> anyhow::Result<(StrategySpec, Option<Arc<dyn SignalFactory>>)> {
    let Some(address) = external else {
        return Ok((StrategySpec::with_params(kind, params), None));
    };
    let address = external::Address::parse(address).map_err(anyhow::Error::msg)?;
    let host = Host::new().map_err(anyhow::Error::msg)?;
    let plugin = host
        .external("external", address)
        .map_err(anyhow::Error::msg)?;
    let spec = StrategySpec::from_info(
        StrategyKind::Plugin(plugin.id.clone()),
        Arc::clone(&plugin.info),
        params,
    );
    Ok((spec, Some(plugin)))
}

fn parse_strategy(id: &str) -> Result<BuiltInStrategy, String> {
    BuiltInStrategy::from_id(id).ok_or_else(|| {
        let ids: Vec<_> = BuiltInStrategy::ALL.iter().map(|s| s.id()).collect();
        format!(
            "unknown strategy `{id}` (expected one of {})",
            ids.join(", ")
        )
    })
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Backtest(args) => backtest(&args),
        Command::Live(args) => live::live(&args),
        Command::Import(args) => data::import(&args),
        Command::Data(command) => data::run(&command),
        Command::Fetch(source) => data::fetch(&source),
        Command::Plugin(command) => plugin::run(&command),
        Command::Models(command) => ml::models(&command),
        Command::Ai(command) => ml::ai(&command),
    }
}

fn backtest(args: &BacktestArgs) -> anyhow::Result<()> {
    let tf = args.timeframe;
    let loaded_at = Instant::now();
    let (instrument, candles, ticks, own_fee) = match &args.local {
        Some(id) => local_market(id, tf, args.days, args.bar_fills)?,
        None => {
            let Some(tf) = tf.timeframe() else {
                anyhow::bail!("Binance serves time bars; tick and volume bars need --local");
            };
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
            let end = tf.open_time(now);
            let start = end.saturating_sub(args.days * NANOS_PER_DAY);
            let binance = BinanceClient::new()?;
            let instrument = binance.instrument(&args.symbol)?;
            let candles = binance
                .klines(&args.symbol, tf, start, end)
                .context("fetching bars from Binance")?;
            (instrument, candles, None, FeeRates::BINANCE_SPOT)
        }
    };
    let load_secs = loaded_at.elapsed().as_secs_f64();
    anyhow::ensure!(
        !candles.is_empty(),
        "there are no bars for {}",
        instrument.symbol
    );

    let (strategy, plugin) = strategy(args.strategy, &args.params, args.external.as_deref())?;
    println!(
        "{}.{} {tf} · {} → {} · {} bars · {strategy} · {}% per position · fills on {}",
        instrument.symbol,
        instrument.venue,
        date(candles[0].ts),
        date(candles[candles.len() - 1].ts),
        candles.len(),
        strategy.size_fraction() * 100.0,
        if ticks.is_some() { "ticks" } else { "bars" },
    );

    let quote = instrument.quote.clone();
    let request = BacktestRequest {
        instrument,
        bars: tf,
        candles,
        strategy,
        plugin,
        ticks,
        starting_balance: args.balance,
        fees: match args.fees {
            None => own_fee,
            Some(Fees::Binance) => FeeRates::BINANCE_SPOT,
            Some(Fees::Zero) => FeeRates::ZERO,
        },
    };

    let run_started = Instant::now();
    let report = spawn_backtest(request)
        .join()
        .map_err(|_| anyhow::anyhow!("backtest thread panicked"))??;
    println!(
        "loaded in {load_secs:.1}s, backtested in {:.2}s\n",
        run_started.elapsed().as_secs_f64()
    );

    print_summary(&report, &quote);
    if args.fills {
        print_fills(&report);
    }
    Ok(())
}

type Loaded = (
    InstrumentSpec,
    Vec<Candle>,
    Option<Arc<dyn TickFeed>>,
    FeeRates,
);

/// The last `days` of a stored market's bars, and its ticks unless `bar_fills`.
fn local_market(id: &str, tf: BarSpec, days: u64, bar_fills: bool) -> anyhow::Result<Loaded> {
    let (symbol, venue) = id
        .rsplit_once('.')
        .with_context(|| format!("`{id}` should be SYMBOL.VENUE"))?;
    let root = Store::default_root().context("no data folder on this system")?;
    let market = Store::new(root).market(&venue.to_uppercase(), &symbol.to_uppercase())?;
    let candles = match tf {
        BarSpec::Time(timeframe) => {
            let max_bars = (days * NANOS_PER_DAY).div_ceil(timeframe.nanos()) as usize;
            market.candles(timeframe, max_bars)?
        }
        // Tick and volume bars come straight from the ticks.
        BarSpec::Ticks(_) | BarSpec::Volume(_) => {
            let to = market.info.last + 1;
            let ticks = market.read_ticks(to.saturating_sub(days * NANOS_PER_DAY), to)?;
            tick::bars_by(&ticks, tf)
        }
    };
    let fee = FeeRates {
        maker: market.info.fee,
        taker: market.info.fee,
    };
    let instrument = market.instrument();
    // A market imported as bars has no ticks to fill on.
    let has_ticks = market.info.kind.ticks().is_some();
    let ticks = (has_ticks && !bar_fills).then(|| Arc::new(market) as Arc<dyn TickFeed>);
    Ok((instrument, candles, ticks, fee))
}

fn print_summary(report: &BacktestReport, quote: &str) {
    let stat = |key: &str| report.stat(key).unwrap_or(f64::NAN);
    println!(
        "  {:<14}{} fills · {} positions",
        "Trades",
        report.fills.len(),
        report.total_positions
    );
    println!(
        "  {:<14}{:+.2} {quote} ({:+.2}%)",
        "Net PnL",
        report.net_pnl(),
        report.net_return() * 100.0
    );
    println!(
        "  {:<14}{:.2}%",
        "Max drawdown",
        report.max_drawdown * 100.0
    );
    println!("  {:<14}{}", "Win rate", percent(stat("Win Rate")));
    println!("  {:<14}{:.2} {quote}", "Fees", report.fees);
    println!();

    print_section("PnL", &report.pnl);
    print_section("Returns", &report.returns);
    print_section("General", &report.general);
    if !report.notes.is_empty() {
        println!("Notes");
        for note in &report.notes {
            println!("  {}{}", if note.warning { "! " } else { "" }, note.text);
        }
        println!();
    }
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
    DateTime::from_nanos(ts).date()
}

/// `YYYY-MM-DD HH:MM` for a UTC timestamp.
fn datetime(ts: UnixNanos) -> String {
    let dt = DateTime::from_nanos(ts);
    format!("{} {}", dt.date(), dt.time())
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

//! `persephone-cli live`: a built-in or external strategy trading Binance bars as they close, on
//! paper, on Binance's testnet, or with real money.

use std::{
    io::{self, BufRead, IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use persephone_core::{Side, Timeframe};
use persephone_data::binance::BinanceClient;
use persephone_engine::{
    FeeRates,
    live::{Book, LiveEvent, LiveFeed, LiveMode, LiveRequest, start_live},
};

use crate::{datetime, parse_strategy, strategy};

/// Bars the strategy sees before going live, to warm up its indicators.
const WARM_UP_BARS: u64 = 1_000;

#[derive(clap::Args)]
pub struct LiveArgs {
    /// Binance spot symbol.
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    /// Bar timeframe: 1m, 5m, 15m, 30m, 1h, 4h or 1d.
    #[arg(long, alias = "bars", default_value = "1m")]
    timeframe: Timeframe,
    /// Strategy: ema-cross, rsi-reversion or donchian-breakout.
    #[arg(long, default_value = "ema-cross", value_parser = parse_strategy)]
    strategy: persephone_core::strategy::BuiltInStrategy,
    /// Strategy parameters in order, comma-separated. Missing ones keep their defaults.
    #[arg(long, value_delimiter = ',')]
    params: Vec<f64>,
    /// Run a strategy in a process of its own instead, listening at this address
    /// (tcp://HOST:PORT or unix://PATH); see plugins/external.
    #[arg(long)]
    external: Option<String>,
    /// The cash positions are sized from, in the quote asset: the paper account's balance, or
    /// how much of a real account the strategy may use.
    #[arg(long, default_value_t = 10_000.0)]
    balance: f64,
    /// Send orders to Binance's spot testnet, with keys from BINANCE_TESTNET_API_KEY and
    /// BINANCE_TESTNET_API_SECRET.
    #[arg(long, conflicts_with = "real_money")]
    testnet: bool,
    /// Send orders to Binance with real money, with keys from BINANCE_API_KEY and
    /// BINANCE_API_SECRET. You'll be asked to confirm.
    #[arg(long)]
    real_money: bool,
}

pub fn live(args: &LiveArgs) -> anyhow::Result<()> {
    let mode = if args.real_money {
        LiveMode::RealMoney
    } else if args.testnet {
        LiveMode::Testnet
    } else {
        LiveMode::Paper
    };
    if let Some((key, secret)) = mode.key_variables() {
        anyhow::ensure!(
            mode.has_keys(),
            "{} trading needs your Binance API key in {key} and its secret in {secret}",
            mode.label()
        );
    }

    let (strategy, plugin) = strategy(args.strategy, &args.params, args.external.as_deref())?;
    let per_position = args.balance * strategy.size_fraction();
    if mode == LiveMode::RealMoney {
        confirm_real_money(&args.symbol, per_position)?;
    }
    let binance = BinanceClient::new()?;
    let instrument = binance.instrument(&args.symbol)?;

    let tf = args.timeframe;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % tf.nanos();
    let history = binance
        .klines(
            &args.symbol,
            tf,
            end.saturating_sub(WARM_UP_BARS * tf.nanos()),
            end,
        )
        .context("fetching recent bars from Binance")?;

    println!(
        "{} {tf} · {strategy} · {:.2} {} per position · {}{}",
        instrument.symbol,
        per_position,
        instrument.quote,
        mode.label(),
        if mode == LiveMode::Paper {
            ""
        } else {
            " (flat instead of short)"
        },
    );
    println!("Connecting. Ctrl-C stops and closes the position.\n");

    let quote = instrument.quote.clone();
    let mut book = Book::default();
    let session = start_live(
        LiveRequest {
            mode,
            feed: LiveFeed::Binance,
            instrument,
            timeframe: tf,
            history,
            strategy,
            plugin,
            balance: args.balance,
            fees: FeeRates::BINANCE_SPOT,
            owns_signals: true,
        },
        move |event| {
            book.apply(&event);
            match event {
                LiveEvent::Started => println!("Connected; waiting for the next bar to close."),
                LiveEvent::Bar(bar) => {
                    println!(
                        "{}  close {:>12.2}   position {:+.5}   PnL {:+.2} {quote}",
                        datetime(bar.ts),
                        bar.close,
                        book.position,
                        book.pnl()
                    );
                }
                LiveEvent::Fill(fill) => {
                    let side = if fill.side == Side::Buy {
                        "BUY "
                    } else {
                        "SELL"
                    };
                    println!(
                        "{}  {side} {} @ {:.2}   fee {:.4} {quote}",
                        datetime(fill.ts),
                        fill.qty,
                        fill.price,
                        fill.fee
                    );
                }
                LiveEvent::Problem(message) => eprintln!("! {message}"),
                LiveEvent::Stopped(None) => println!("\nStopped. PnL {:+.2} {quote}", book.pnl()),
                LiveEvent::Stopped(Some(error)) => eprintln!("\nStopped: {error}"),
            }
        },
    )?;
    session.join();
    Ok(())
}

/// Real money only goes ahead once the person at the terminal types back what's at stake.
fn confirm_real_money(symbol: &str, per_position: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        io::stdin().is_terminal(),
        "real-money trading has to be confirmed at a terminal"
    );
    let phrase = format!("trade {symbol} with real money");
    println!("This sends real orders to Binance with your money: positions worth about");
    println!(
        "{per_position:.2} of {symbol}'s quote asset, for as long as it runs. Losses are real."
    );
    print!("Type `{phrase}` to go ahead: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    anyhow::ensure!(answer.trim() == phrase, "not confirmed; nothing was sent");
    Ok(())
}

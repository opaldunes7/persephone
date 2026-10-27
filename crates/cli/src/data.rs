//! `persephone-cli import` and `persephone-cli data`: CSV tick files into the local store, and
//! looking after what's there.

use std::{
    io::Write as _,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};
use persephone_core::{
    UnixNanos,
    tick::TickKind,
    time::{DateTime, NANOS_PER_DAY},
};
use persephone_data::{
    archive,
    import::{
        self, DEFAULT_VENUE, Detection, Format, MarketOptions, Overrides, Progress, Role, Tick,
        TimeFormat, Zone,
    },
    store::Store,
};

#[derive(Args)]
pub struct ImportArgs {
    /// CSV files (plain, .gz or .zip), all laid out the same way.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// The market's symbol, e.g. EURUSD or BTCUSDT.
    #[arg(long)]
    symbol: String,
    /// Where the data came from; markets are named SYMBOL.VENUE.
    #[arg(long, default_value = DEFAULT_VENUE)]
    venue: String,
    /// The base currency, when the symbol doesn't make it obvious.
    #[arg(long)]
    base: Option<String>,
    /// The quote currency, when the symbol doesn't make it obvious.
    #[arg(long)]
    quote: Option<String>,
    /// Fee rate per backtest fill (default 0.001 for trades, 0 for quotes).
    #[arg(long)]
    fee: Option<f64>,
    /// Time zone of timestamps written without one: UTC, -05:00 or America/New_York.
    #[arg(long)]
    tz: Option<String>,
    /// Use a saved layout instead of detecting one.
    #[arg(long, conflicts_with_all = ["kind", "delimiter", "header", "no_header", "time_format"])]
    profile: Option<String>,
    /// Save the layout used under this name, for `--profile`.
    #[arg(long)]
    save_profile: Option<String>,
    /// Show what was detected and stop.
    #[arg(long)]
    dry_run: bool,

    /// Trades or quotes, if detection gets it wrong.
    #[arg(long, value_enum)]
    kind: Option<Kind>,
    /// The column separator.
    #[arg(long)]
    delimiter: Option<char>,
    /// The first line is a header.
    #[arg(long)]
    header: bool,
    /// The first line is data.
    #[arg(long, conflicts_with = "header")]
    no_header: bool,
    /// How times are written.
    #[arg(long, value_enum)]
    time_format: Option<TimeFormatArg>,
    /// Columns by header name or number from 1.
    #[arg(long)]
    time: Option<String>,
    #[arg(long)]
    date: Option<String>,
    #[arg(long)]
    price: Option<String>,
    #[arg(long)]
    size: Option<String>,
    #[arg(long)]
    side: Option<String>,
    #[arg(long)]
    bid: Option<String>,
    #[arg(long)]
    ask: Option<String>,
    #[arg(long)]
    bid_size: Option<String>,
    #[arg(long)]
    ask_size: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Trades,
    Quotes,
}

#[derive(Clone, Copy, ValueEnum)]
enum TimeFormatArg {
    EpochSeconds,
    EpochMillis,
    EpochMicros,
    EpochNanos,
    YearFirst,
    DayFirst,
    MonthFirst,
}

#[derive(Subcommand)]
pub enum DataCommand {
    /// List the markets in the local store.
    List,
    /// Delete a market and all its ticks.
    Remove {
        /// SYMBOL.VENUE, as `data list` shows it.
        market: String,
        /// Really delete it.
        #[arg(long)]
        yes: bool,
    },
    /// Print the store's folder.
    Dir,
}

pub fn import(args: &ImportArgs) -> anyhow::Result<()> {
    let store = store()?;
    let zone: Option<Zone> = args.tz.as_deref().map(str::parse).transpose()?;
    let format = match &args.profile {
        Some(name) => {
            let mut format = import::load_profile(&profiles_dir()?, name)?;
            if let Some(zone) = zone {
                format.zone = zone;
            }
            println!("Using profile `{name}` ({})", format.kind);
            format
        }
        None => {
            let sample = import::sample(&args.files[0])?;
            let detection =
                import::detect(&sample, &overrides(args, zone)?).with_context(|| {
                    format!("working out the layout of {}", args.files[0].display())
                })?;
            show(&detection);
            detection.format
        }
    };
    if let Some(name) = &args.save_profile {
        let path = import::save_profile(&profiles_dir()?, name, &format)?;
        println!("Saved the layout as profile `{name}` ({})", path.display());
    }
    if args.dry_run {
        println!("\nDry run: nothing imported.");
        return Ok(());
    }

    let options = MarketOptions {
        symbol: args.symbol.to_uppercase(),
        venue: args.venue.to_uppercase(),
        base: args.base.as_ref().map(|b| b.to_uppercase()),
        quote: args.quote.as_ref().map(|q| q.to_uppercase()),
        fee: args.fee,
    };
    import_files(&store, &args.files, &format, &options)
}

/// Streams `files` into the store with progress, then says what's there.
fn import_files(
    store: &Store,
    files: &[PathBuf],
    format: &Format,
    options: &MarketOptions,
) -> anyhow::Result<()> {
    println!(
        "\nImporting {} file(s) into {}.{} …",
        files.len(),
        options.symbol,
        options.venue
    );
    let started = Instant::now();
    let mut last_shown = Instant::now() - Duration::from_secs(1);
    let mut show_progress = |p: &Progress| {
        if last_shown.elapsed() >= Duration::from_millis(250) {
            last_shown = Instant::now();
            let percent = if p.total > 0 {
                p.read as f64 / p.total as f64 * 100.0
            } else {
                0.0
            };
            let rate = p.rows as f64 / started.elapsed().as_secs_f64().max(1e-3);
            eprint!(
                "\r  {percent:5.1}%  {} rows  {}/s   ",
                count(p.rows),
                count(rate as u64)
            );
            let _ = std::io::stderr().flush();
        }
        true
    };
    let summary = import::import(store, files, format, options, &mut show_progress)?;
    eprintln!("\r{:60}\r", "");

    let market = &summary.market;
    let days = (summary.last.saturating_sub(summary.first)) / NANOS_PER_DAY + 1;
    println!(
        "Imported {} {} into {} in {:.1}s: {} → {} ({} days)",
        count(summary.imported),
        format.kind,
        market.id(),
        started.elapsed().as_secs_f64(),
        timestamp(summary.first),
        timestamp(summary.last),
        count(days),
    );
    println!(
        "{} now holds {} {} from {} to {}; prices to {} decimals.",
        market.id(),
        count(market.info.count),
        market.info.kind,
        DateTime::from_nanos(market.info.first).date(),
        DateTime::from_nanos(market.info.last).date(),
        market.info.price_precision,
    );
    if summary.skipped > 0 {
        println!("Skipped {} rows that didn't fit:", count(summary.skipped));
        for problem in &summary.problems {
            println!("  {problem}");
        }
    }
    println!("Open it from any chart: click → Symbol → Local.");
    Ok(())
}

#[derive(Subcommand)]
pub enum FetchSource {
    /// Binance's monthly or daily trade archives (data.binance.vision), downloaded, checked
    /// against their published checksums and imported as SYMBOL.BINANCE.
    Binance {
        /// A Binance spot symbol, e.g. BTCUSDT.
        symbol: String,
        /// The first month (2024-01) or day (2024-01-15).
        #[arg(long)]
        from: String,
        /// The last month or day; just the first if left out.
        #[arg(long)]
        to: Option<String>,
        /// Every single trade, instead of trades merged by price and taker (much bigger).
        #[arg(long)]
        all_trades: bool,
        /// Keep the downloaded zip files.
        #[arg(long)]
        keep: bool,
        #[arg(long, hide = true, default_value = archive::ARCHIVE_URL)]
        archive_url: String,
    },
}

pub fn fetch(source: &FetchSource) -> anyhow::Result<()> {
    let FetchSource::Binance {
        symbol,
        from,
        to,
        all_trades,
        keep,
        archive_url,
    } = source;
    let symbol = symbol.to_uppercase();
    let periods = archive::periods(from, to.as_deref().unwrap_or(from))?;
    let kind = if *all_trades {
        archive::Kind::Trades
    } else {
        archive::Kind::AggTrades
    };
    let downloads = dirs::data_dir()
        .context("no data folder on this system")?
        .join("persephone")
        .join("downloads");
    // Monthly archives run to gigabytes: no overall time limit.
    let http = reqwest::blocking::Client::builder()
        .user_agent(concat!("persephone/", env!("CARGO_PKG_VERSION")))
        .timeout(None)
        .build()?;

    let mut files = Vec::new();
    for period in periods {
        let name = archive::file_name(&symbol, kind, period);
        let dest = downloads.join(&name);
        if dest.exists() {
            println!("{name}: already downloaded");
            files.push(dest);
            continue;
        }
        let mut shown = Instant::now() - Duration::from_secs(1);
        let mut progress = |done: u64, total: Option<u64>| {
            if shown.elapsed() >= Duration::from_millis(250) {
                shown = Instant::now();
                let of = total.map_or(String::new(), |t| format!(" of {} MB", t >> 20));
                eprint!("\r{name}: {} MB{of}   ", done >> 20);
                let _ = std::io::stderr().flush();
            }
        };
        let url = archive::url(archive_url, &symbol, kind, period);
        if archive::download(&http, &url, &dest, &mut progress)? {
            eprintln!("\r{:70}\r{name}: downloaded and checked", "");
            files.push(dest);
        } else {
            eprintln!(
                "\r{:70}\r{name}: not published (no trades then, or not out yet)",
                ""
            );
        }
    }
    anyhow::ensure!(
        !files.is_empty(),
        "none of those archives exist; check the symbol and dates"
    );

    let sample = import::sample(&files[0])?;
    let detection = import::detect(&sample, &Overrides::default())?;
    show(&detection);
    let options = MarketOptions {
        symbol,
        venue: "BINANCE".into(),
        base: None,
        quote: None,
        fee: None,
    };
    let result = import_files(&store()?, &files, &detection.format, &options);
    if !keep {
        for file in &files {
            let _ = std::fs::remove_file(file);
        }
    }
    result
}

fn overrides(args: &ImportArgs, zone: Option<Zone>) -> anyhow::Result<Overrides> {
    let mut columns = Vec::new();
    for (role, spec) in [
        (Role::Time, &args.time),
        (Role::Date, &args.date),
        (Role::Price, &args.price),
        (Role::Size, &args.size),
        (Role::Side, &args.side),
        (Role::Bid, &args.bid),
        (Role::Ask, &args.ask),
        (Role::BidSize, &args.bid_size),
        (Role::AskSize, &args.ask_size),
    ] {
        if let Some(spec) = spec {
            columns.push((role, spec.clone()));
        }
    }
    let delimiter = match args.delimiter {
        Some('t') => Some('\t'),
        other => other,
    };
    if delimiter.is_some_and(|d| !d.is_ascii()) {
        bail!("the separator must be one ASCII character");
    }
    Ok(Overrides {
        delimiter,
        header: if args.header {
            Some(true)
        } else if args.no_header {
            Some(false)
        } else {
            None
        },
        kind: args.kind.map(|k| match k {
            Kind::Trades => TickKind::Trades,
            Kind::Quotes => TickKind::Quotes,
        }),
        time_format: args.time_format.map(|f| match f {
            TimeFormatArg::EpochSeconds => TimeFormat::EpochSeconds,
            TimeFormatArg::EpochMillis => TimeFormat::EpochMillis,
            TimeFormatArg::EpochMicros => TimeFormat::EpochMicros,
            TimeFormatArg::EpochNanos => TimeFormat::EpochNanos,
            TimeFormatArg::YearFirst => TimeFormat::YearFirst,
            TimeFormatArg::DayFirst => TimeFormat::DayFirst,
            TimeFormatArg::MonthFirst => TimeFormat::MonthFirst,
        }),
        zone,
        columns,
    })
}

fn show(detection: &Detection) {
    println!("Detected: {}", detection.describe().trim_end());
    for note in &detection.notes {
        println!("Note: {note}");
    }
    if detection.zone_assumed {
        println!(
            "Note: the times don't say their zone, so they're read as UTC; use --tz if they aren't"
        );
    }
    if detection.bad_rows > 0 {
        println!(
            "{} of the first {} rows don't fit and will be skipped.",
            detection.bad_rows, detection.rows
        );
    }
    println!("First rows:");
    for tick in &detection.preview {
        println!("  {}", describe_tick(tick, &detection.format));
    }
}

fn describe_tick(tick: &Tick, format: &Format) -> String {
    match tick {
        Tick::Trade(t) => {
            let side = match t.aggressor {
                persephone_core::tick::Aggressor::Buyer => "  buyer took",
                persephone_core::tick::Aggressor::Seller => "  seller took",
                persephone_core::tick::Aggressor::Unknown => "",
            };
            let size = if format.columns.size.is_some() {
                format!("  size {}", t.size)
            } else {
                String::new()
            };
            format!("{}  price {}{size}{side}", timestamp(t.ts), t.price)
        }
        Tick::Quote(q) => format!("{}  bid {}  ask {}", timestamp(q.ts), q.bid, q.ask),
    }
}

pub fn run(command: &DataCommand) -> anyhow::Result<()> {
    let store = store()?;
    match command {
        DataCommand::List => {
            let markets = store.markets();
            if markets.is_empty() {
                println!(
                    "The store is empty. Import CSV ticks with `persephone-cli import FILE --symbol SYMBOL`."
                );
            }
            for market in markets {
                let info = &market.info;
                println!(
                    "{:<24} {:<7} {:>14}  {} → {}  fee {}%",
                    market.id(),
                    info.kind,
                    count(info.count),
                    DateTime::from_nanos(info.first).date(),
                    DateTime::from_nanos(info.last).date(),
                    info.fee * 100.0,
                );
            }
        }
        DataCommand::Remove { market, yes } => {
            let (symbol, venue) = market
                .rsplit_once('.')
                .with_context(|| format!("`{market}` should be SYMBOL.VENUE"))?;
            let found = store.market(venue, symbol)?;
            if *yes {
                store.remove(venue, symbol)?;
                println!("Deleted {}.", found.id());
            } else {
                println!(
                    "This deletes {} ({} {}) for good. Run again with --yes to do it.",
                    found.id(),
                    count(found.info.count),
                    found.info.kind
                );
            }
        }
        DataCommand::Dir => println!("{}", store.root().display()),
    }
    Ok(())
}

fn store() -> anyhow::Result<Store> {
    Ok(Store::new(
        Store::default_root().context("no data folder on this system")?,
    ))
}

fn profiles_dir() -> anyhow::Result<PathBuf> {
    import::profiles_dir().context("no data folder on this system")
}

/// `2014-01-02 03:04:05.378` (UTC).
fn timestamp(ts: UnixNanos) -> String {
    let dt = DateTime::from_nanos(ts);
    let seconds = ts / 1_000_000_000 % 60;
    let millis = ts / 1_000_000 % 1_000;
    format!("{} {}:{seconds:02}.{millis:03}", dt.date(), dt.time())
}

/// `12345678` → `12,345,678`.
fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts_and_times() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(12_345_678), "12,345,678");
        assert_eq!(
            timestamp(1_388_631_845_378_000_000),
            "2014-01-02 03:04:05.378"
        );
    }
}

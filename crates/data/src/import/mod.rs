//! Importing ticks or bars from CSV files into the [store](crate::store).
//!
//! [`detect`] works out a file's layout from its first lines: the separator, whether there's
//! a header, which columns hold the time, prices (or open, high, low and close), sizes and
//! aggressor side, and how the time is written. [`import`] then streams any number of files in
//! that layout into a market a day (or, for bars, a year) at a time, so files bigger than
//! memory are fine. A layout can be saved as a named profile and reused.

mod detect;
mod source;
pub mod time;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, bail, ensure};
use csv::ByteRecord;
use persephone_core::{
    Candle, UnixNanos,
    tick::{Aggressor, Quote, TickKind, Ticks, Trade},
};
use serde::{Deserialize, Serialize};

pub use self::{
    detect::{Detection, Overrides, Role, detect},
    source::sample,
    time::{TimeFormat, Zone},
};
use crate::store::{
    BarWriter, LocalMarket, MarketInfo, MarketKind, MarketWriter, Spacing, Store, check_name,
    day_of, year_of,
};

/// The venue imported markets get unless told otherwise.
pub const DEFAULT_VENUE: &str = "LOCAL";

/// How a CSV file lays out its ticks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Format {
    pub delimiter: char,
    /// The first line names the columns.
    pub header: bool,
    /// Numbers are written with a decimal comma (`1,2345`).
    #[serde(default)]
    pub decimal_comma: bool,
    pub kind: Kind,
    pub columns: Columns,
    pub time_format: TimeFormat,
    /// The zone of timestamps written without an offset.
    #[serde(default)]
    pub zone: Zone,
}

/// What a file's rows are: trades, quotes, or bars (open, high, low, close and maybe volume).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Trades,
    Quotes,
    Bars,
}

impl Kind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trades => "trades",
            Self::Quotes => "quotes",
            Self::Bars => "bars",
        }
    }

    /// The kind of tick, unless these are bars.
    #[must_use]
    pub const fn ticks(self) -> Option<TickKind> {
        match self {
            Self::Trades => Some(TickKind::Trades),
            Self::Quotes => Some(TickKind::Quotes),
            Self::Bars => None,
        }
    }
}

impl From<TickKind> for Kind {
    fn from(kind: TickKind) -> Self {
        match kind {
            TickKind::Trades => Self::Trades,
            TickKind::Quotes => Self::Quotes,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Which column holds what, counting from 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Columns {
    /// The timestamp, or the time of day when `date` is set.
    pub time: usize,
    /// A separate date column, read in front of `time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<usize>,
    #[serde(default)]
    pub side_format: SideFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_size: Option<usize>,
    /// A bar's prices; its volume is in `size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<usize>,
}

/// How a side column names the aggressor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SideFormat {
    /// `buy` / `sell` or `b` / `s`, any case.
    #[default]
    Words,
    /// `true` when the buyer was the maker, so the seller took liquidity (Binance's
    /// `is_buyer_maker`).
    BuyerIsMaker,
    /// `1` buyer, `-1` seller, `0` unknown.
    Sign,
}

/// One parsed row: a tick, or a bar keyed by the time written in the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tick {
    Trade(Trade),
    Quote(Quote),
    Bar(Candle),
}

impl Tick {
    #[must_use]
    pub const fn ts(&self) -> UnixNanos {
        match self {
            Self::Trade(t) => t.ts,
            Self::Quote(q) => q.ts,
            Self::Bar(b) => b.ts,
        }
    }
}

/// A parsed row plus the decimals its numbers were written with.
struct Parsed {
    tick: Tick,
    price_decimals: u8,
    size_decimals: u8,
}

/// Turns CSV rows into ticks.
struct RowParser<'a> {
    format: &'a Format,
    scratch: String,
}

impl<'a> RowParser<'a> {
    fn new(format: &'a Format) -> Self {
        Self {
            format,
            scratch: String::new(),
        }
    }

    fn parse(&mut self, record: &ByteRecord) -> Result<Parsed, String> {
        let format = self.format;
        let columns = &format.columns;
        let ts = match columns.date {
            Some(date) => {
                let mut joined = std::mem::take(&mut self.scratch);
                joined.clear();
                joined.push_str(field(record, date, "date")?);
                joined.push(' ');
                joined.push_str(field(record, columns.time, "time")?);
                let ts = time::parse(&joined, format.time_format, &format.zone);
                self.scratch = joined;
                ts
            }
            None => time::parse(
                field(record, columns.time, "time")?,
                format.time_format,
                &format.zone,
            ),
        }
        .map_err(|e| format!("the time isn't readable ({e})"))?;

        match format.kind {
            Kind::Trades => {
                let column = columns.price.ok_or("no price column")?;
                let (price, price_decimals) = self.number(record, column, "price")?;
                if price <= 0.0 {
                    return Err(format!("the price {price} isn't positive"));
                }
                let (mut size, size_decimals) = match columns.size {
                    Some(column) => self.number(record, column, "size")?,
                    None => (0.0, 0),
                };
                let mut aggressor = match columns.side {
                    Some(column) => side(field(record, column, "side")?, columns.side_format)?,
                    None => Aggressor::Unknown,
                };
                // Some feeds sign the size instead: negative for sells.
                if size < 0.0 {
                    size = -size;
                    if columns.side.is_none() {
                        aggressor = Aggressor::Seller;
                    }
                }
                Ok(Parsed {
                    tick: Tick::Trade(Trade {
                        ts,
                        price,
                        size,
                        aggressor,
                    }),
                    price_decimals,
                    size_decimals,
                })
            }
            Kind::Quotes => {
                let (bid, bid_decimals) =
                    self.number(record, columns.bid.ok_or("no bid column")?, "bid")?;
                let (ask, ask_decimals) =
                    self.number(record, columns.ask.ok_or("no ask column")?, "ask")?;
                if bid <= 0.0 || ask <= 0.0 {
                    return Err("bid and ask must be positive".into());
                }
                let mut size = |column: Option<usize>, what| match column {
                    Some(column) => self.number(record, column, what).map(|(v, d)| (v.abs(), d)),
                    None => Ok((0.0, 0)),
                };
                let (bid_size, bid_size_decimals) = size(columns.bid_size, "bid size")?;
                let (ask_size, ask_size_decimals) = size(columns.ask_size, "ask size")?;
                Ok(Parsed {
                    tick: Tick::Quote(Quote {
                        ts,
                        bid,
                        ask,
                        bid_size,
                        ask_size,
                    }),
                    price_decimals: bid_decimals.max(ask_decimals),
                    size_decimals: bid_size_decimals.max(ask_size_decimals),
                })
            }
            Kind::Bars => {
                let mut price = |column: Option<usize>, what| {
                    self.number(record, column.ok_or(format!("no {what} column"))?, what)
                };
                let (open, open_decimals) = price(columns.open, "open")?;
                let (high, high_decimals) = price(columns.high, "high")?;
                let (low, low_decimals) = price(columns.low, "low")?;
                let (close, close_decimals) = price(columns.close, "close")?;
                if open <= 0.0 || high <= 0.0 || low <= 0.0 || close <= 0.0 {
                    return Err("open, high, low and close must be positive".into());
                }
                if high < low {
                    return Err(format!("the high {high} is below the low {low}"));
                }
                let (volume, size_decimals) = match columns.size {
                    Some(column) => self
                        .number(record, column, "volume")
                        .map(|(v, d)| (v.abs(), d))?,
                    None => (0.0, 0),
                };
                Ok(Parsed {
                    // Rounding in some exports leaves the open or close a hair outside.
                    tick: Tick::Bar(Candle {
                        ts,
                        open,
                        high: high.max(open).max(close),
                        low: low.min(open).min(close),
                        close,
                        volume,
                    }),
                    price_decimals: open_decimals
                        .max(high_decimals)
                        .max(low_decimals)
                        .max(close_decimals),
                    size_decimals,
                })
            }
        }
    }

    /// A number and how many decimals it was written with (trailing zeros don't count).
    fn number(
        &mut self,
        record: &ByteRecord,
        column: usize,
        what: &str,
    ) -> Result<(f64, u8), String> {
        let text = field(record, column, what)?;
        let text = if self.format.decimal_comma && text.contains(',') {
            self.scratch.clear();
            self.scratch.push_str(&text.replace(',', "."));
            self.scratch.as_str()
        } else {
            text
        };
        let value: f64 = text
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .ok_or_else(|| format!("the {what} `{text}` isn't a number"))?;
        Ok((value, decimals(text)))
    }
}

fn field<'r>(record: &'r ByteRecord, column: usize, what: &str) -> Result<&'r str, String> {
    let bytes = record
        .get(column)
        .ok_or_else(|| format!("the row has no column {} for the {what}", column + 1))?;
    std::str::from_utf8(bytes)
        .map(str::trim)
        .map_err(|_| format!("the {what} isn't plain text"))
}

fn side(text: &str, format: SideFormat) -> Result<Aggressor, String> {
    let is = |options: &[&str]| options.iter().any(|o| text.eq_ignore_ascii_case(o));
    let aggressor = match format {
        SideFormat::Words if is(&["buy", "b"]) => Aggressor::Buyer,
        SideFormat::Words if is(&["sell", "s"]) => Aggressor::Seller,
        SideFormat::BuyerIsMaker if is(&["true", "t", "1", "yes"]) => Aggressor::Seller,
        SideFormat::BuyerIsMaker if is(&["false", "f", "0", "no"]) => Aggressor::Buyer,
        SideFormat::Sign if is(&["1", "+1"]) => Aggressor::Buyer,
        SideFormat::Sign if is(&["-1"]) => Aggressor::Seller,
        _ if text.is_empty() || is(&["0"]) => Aggressor::Unknown,
        _ => return Err(format!("the side `{text}` isn't one this file uses")),
    };
    Ok(aggressor)
}

/// Decimals written after the point, ignoring trailing zeros, at most 9.
fn decimals(text: &str) -> u8 {
    let digits = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.trim_end_matches('0').len());
    digits.min(9) as u8
}

/// What to call an imported market, and how to trade it.
#[derive(Clone, Debug)]
pub struct MarketOptions {
    pub symbol: String,
    pub venue: String,
    /// Worked out from the symbol when not given (`EURUSD` → EUR / USD).
    pub base: Option<String>,
    pub quote: Option<String>,
    /// Fee rate per fill; 0.1% for trades and bars, and 0 for quotes (the spread is the cost)
    /// when not given.
    pub fee: Option<f64>,
}

/// How an import is going.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    /// Bytes read from disk, of `total` (compressed bytes for packed files).
    pub read: u64,
    pub total: u64,
    pub rows: u64,
}

/// What an import did.
#[derive(Clone, Debug)]
pub struct Summary {
    pub market: LocalMarket,
    pub imported: u64,
    pub skipped: u64,
    /// The first few rows that didn't parse, with where they are.
    pub problems: Vec<String>,
    /// The imported ticks' time span.
    pub first: UnixNanos,
    pub last: UnixNanos,
    /// Stopped early by the progress callback.
    pub stopped: bool,
}

/// How many skipped rows to describe.
const PROBLEMS_SHOWN: usize = 8;
/// Ticks buffered before everything held is written out, to bound memory on unsorted files.
const MAX_HELD: usize = 20_000_000;

/// Streams `files` (all in `format`) into the market named by `options`, creating it if
/// needed. `progress` is called now and then; returning `false` stops the import, keeping
/// what was read so far.
///
/// # Errors
///
/// Returns an error if a file can't be read, most of a file's rows don't match the format,
/// the market holds something else (the other kind of tick, or bars of another spacing), or
/// the store can't be written.
pub fn import(
    store: &Store,
    files: &[PathBuf],
    format: &Format,
    options: &MarketOptions,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> anyhow::Result<Summary> {
    ensure!(!files.is_empty(), "no files to import");
    let kind = market_kind(store, files, format, options)?;
    let market = open_market(store, kind, options)?;
    let mut sink = Sink::new(market)?;
    let mut stats = Stats::default();

    let total = files
        .iter()
        .map(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0))
        .sum();
    let read = Arc::new(AtomicU64::new(0));
    let mut stopped = false;

    for path in files {
        if stopped {
            break;
        }
        source::for_each_csv(path, &read, |reader, name| {
            let mut csv = csv::ReaderBuilder::new()
                .delimiter(format.delimiter as u8)
                .has_headers(false)
                .flexible(true)
                .buffer_capacity(1 << 20)
                .from_reader(reader);
            let mut record = ByteRecord::new();
            let mut parser = RowParser::new(format);
            let (mut good, mut bad) = (0u64, 0u64);
            let mut first_row = true;

            while csv
                .read_byte_record(&mut record)
                .with_context(|| format!("reading {name}"))?
            {
                if record.iter().all(|f| f.iter().all(u8::is_ascii_whitespace)) {
                    continue;
                }
                match parser.parse(&record) {
                    Ok(parsed) => {
                        good += 1;
                        stats.add(&parsed);
                        sink.push(parsed.tick)?;
                    }
                    // A header can repeat at the top of every file.
                    Err(_) if first_row && format.header => {}
                    Err(reason) => {
                        bad += 1;
                        let line = record.position().map_or(0, csv::Position::line);
                        stats.problem(format!("{name} line {line}: {reason}"));
                    }
                }
                first_row = false;

                if good + bad == 1_000 && bad > good {
                    bail!(
                        "most rows of {name} don't fit the format; the first problem was {}",
                        stats.problems.first().map_or("unknown", String::as_str)
                    );
                }
                if (good + bad) % 65_536 == 0 {
                    let now = Progress {
                        read: read.load(Ordering::Relaxed),
                        total,
                        rows: stats.imported + stats.skipped,
                    };
                    if !progress(&now) {
                        stopped = true;
                        return Ok(());
                    }
                }
            }
            Ok(())
        })?;
    }

    let mut market = sink.finish()?;
    market.info.price_precision = market.info.price_precision.max(stats.price_decimals);
    market.info.size_precision = market.info.size_precision.max(stats.size_decimals);
    market.save_info()?;
    progress(&Progress {
        read: total,
        total,
        rows: stats.imported + stats.skipped,
    });

    Ok(Summary {
        market,
        imported: stats.imported,
        skipped: stats.skipped,
        problems: stats.problems,
        first: stats.first.unwrap_or(0),
        last: stats.last,
        stopped,
    })
}

/// What the files will make: their kind of tick, or bars spaced as the first file's are.
fn market_kind(
    store: &Store,
    files: &[PathBuf],
    format: &Format,
    options: &MarketOptions,
) -> anyhow::Result<MarketKind> {
    if let Some(kind) = format.kind.ticks() {
        return Ok(MarketKind::Ticks(kind));
    }
    let times = sample_times(&sample(&files[0])?, format);
    match Spacing::from_times(&times) {
        Some((spacing, _)) => Ok(MarketKind::Bars(spacing)),
        // One bar can still join a market that has bars.
        None => store
            .market(&options.venue, &options.symbol)
            .ok()
            .map(|market| market.info.kind)
            .filter(|kind| kind.spacing().is_some())
            .with_context(|| {
                format!(
                    "can't tell how far apart the bars in {} are; it needs at least two",
                    files[0].display()
                )
            }),
    }
}

/// The times of the rows in a file's sample that `format` reads.
#[must_use]
pub fn sample_times(sample: &str, format: &Format) -> Vec<UnixNanos> {
    let mut csv = csv::ReaderBuilder::new()
        .delimiter(format.delimiter as u8)
        .has_headers(format.header)
        .flexible(true)
        .from_reader(sample.as_bytes());
    let mut parser = RowParser::new(format);
    let mut record = ByteRecord::new();
    let mut times = Vec::new();
    while let Ok(true) = csv.read_byte_record(&mut record) {
        if let Ok(parsed) = parser.parse(&record) {
            times.push(parsed.tick.ts());
        }
    }
    times
}

fn open_market(
    store: &Store,
    kind: MarketKind,
    options: &MarketOptions,
) -> anyhow::Result<LocalMarket> {
    check_name("symbol", &options.symbol)?;
    check_name("venue", &options.venue)?;
    if let Ok(mut market) = store.market(&options.venue, &options.symbol) {
        ensure!(
            market.info.kind == kind,
            "{} holds {}, but these files have {kind}",
            market.id(),
            market.info.kind
        );
        if let Some(fee) = options.fee {
            market.info.fee = fee;
            market.save_info()?;
        }
        return Ok(market);
    }
    let (base, quote) = match (&options.base, &options.quote) {
        (Some(base), Some(quote)) => (base.clone(), quote.clone()),
        _ => split_symbol(&options.symbol).with_context(|| {
            format!(
                "can't tell the base and quote currencies of {}; give them with --base and --quote",
                options.symbol
            )
        })?,
    };
    store.create(MarketInfo {
        symbol: options.symbol.clone(),
        venue: options.venue.clone(),
        base,
        quote,
        price_precision: 0,
        size_precision: 0,
        kind,
        fee: options.fee.unwrap_or(match kind {
            MarketKind::Ticks(TickKind::Quotes) => 0.0,
            MarketKind::Ticks(TickKind::Trades) | MarketKind::Bars(_) => 0.001,
        }),
        first: 0,
        last: 0,
        count: 0,
    })
}

/// `EURUSD` → (EUR, USD), `BTCUSDT` → (BTC, USDT): six letters split in half, otherwise a
/// known quote currency at the end.
#[must_use]
pub fn split_symbol(symbol: &str) -> Option<(String, String)> {
    const QUOTES: [&str; 17] = [
        "FDUSD", "USDT", "USDC", "BUSD", "TUSD", "USD", "EUR", "GBP", "JPY", "CHF", "AUD", "CAD",
        "NZD", "BTC", "ETH", "BNB", "TRY",
    ];
    if symbol.len() == 6 && symbol.chars().all(|c| c.is_ascii_uppercase()) {
        return Some((symbol[..3].to_owned(), symbol[3..].to_owned()));
    }
    QUOTES.iter().find_map(|quote| {
        let base = symbol.strip_suffix(quote)?;
        (!base.is_empty() && base.chars().all(|c| c.is_ascii_alphanumeric()))
            .then(|| (base.to_owned(), (*quote).to_owned()))
    })
}

#[derive(Default)]
struct Stats {
    imported: u64,
    skipped: u64,
    problems: Vec<String>,
    price_decimals: u8,
    size_decimals: u8,
    first: Option<UnixNanos>,
    last: UnixNanos,
}

impl Stats {
    fn add(&mut self, parsed: &Parsed) {
        self.imported += 1;
        self.price_decimals = self.price_decimals.max(parsed.price_decimals);
        self.size_decimals = self.size_decimals.max(parsed.size_decimals);
        let ts = parsed.tick.ts();
        self.first = Some(self.first.map_or(ts, |first| first.min(ts)));
        self.last = self.last.max(ts);
    }

    fn problem(&mut self, description: String) {
        self.skipped += 1;
        if self.problems.len() < PROBLEMS_SHOWN {
            self.problems.push(description);
        }
    }
}

/// Where parsed rows go: ticks a day at a time, or bars a year at a time.
enum Sink {
    Ticks(DayBuffer, MarketWriter),
    Bars(YearBuffer, BarWriter),
}

impl Sink {
    fn new(market: LocalMarket) -> anyhow::Result<Self> {
        Ok(match market.info.kind {
            MarketKind::Ticks(kind) => Self::Ticks(DayBuffer::new(kind), MarketWriter::new(market)),
            MarketKind::Bars(spacing) => {
                Self::Bars(YearBuffer::new(spacing), BarWriter::new(market)?)
            }
        })
    }

    fn push(&mut self, tick: Tick) -> anyhow::Result<()> {
        match self {
            Self::Ticks(days, writer) => days.push(tick, writer),
            Self::Bars(years, writer) => years.push(tick, writer),
        }
    }

    fn finish(self) -> anyhow::Result<LocalMarket> {
        match self {
            Self::Ticks(mut days, mut writer) => {
                days.flush(&mut writer)?;
                writer.finish()
            }
            Self::Bars(mut years, mut writer) => {
                years.flush(&mut writer)?;
                writer.finish()
            }
        }
    }
}

/// Holds bars by UTC year (of their open, by the market's spacing) until the file has moved
/// past that year.
struct YearBuffer {
    spacing: Spacing,
    years: BTreeMap<i64, Vec<Candle>>,
    held: usize,
    newest: Option<i64>,
}

impl YearBuffer {
    fn new(spacing: Spacing) -> Self {
        Self {
            spacing,
            years: BTreeMap::new(),
            held: 0,
            newest: None,
        }
    }

    fn push(&mut self, tick: Tick, writer: &mut BarWriter) -> anyhow::Result<()> {
        let Tick::Bar(mut bar) = tick else {
            bail!("a file mixed bars and ticks");
        };
        bar.ts = self.spacing.open_time(bar.ts);
        let year = year_of(bar.ts);
        self.years.entry(year).or_default().push(bar);
        self.held += 1;

        if self.newest.is_none_or(|newest| year > newest) {
            self.newest = Some(year);
            let kept = self.years.split_off(&(year - 1));
            for (done, bars) in std::mem::replace(&mut self.years, kept) {
                self.held -= bars.len();
                writer.write_year(done, bars)?;
            }
        }
        if self.held >= MAX_HELD {
            self.flush(writer)?;
        }
        Ok(())
    }

    fn flush(&mut self, writer: &mut BarWriter) -> anyhow::Result<()> {
        for (year, bars) in std::mem::take(&mut self.years) {
            writer.write_year(year, bars)?;
        }
        self.held = 0;
        Ok(())
    }
}

/// Holds ticks by UTC day until the file has moved past that day.
struct DayBuffer {
    kind: TickKind,
    days: BTreeMap<i64, Ticks>,
    held: usize,
    newest: Option<i64>,
}

impl DayBuffer {
    fn new(kind: TickKind) -> Self {
        Self {
            kind,
            days: BTreeMap::new(),
            held: 0,
            newest: None,
        }
    }

    fn push(&mut self, tick: Tick, writer: &mut MarketWriter) -> anyhow::Result<()> {
        let day = day_of(tick.ts());
        match (
            self.days
                .entry(day)
                .or_insert_with(|| Ticks::empty(self.kind)),
            tick,
        ) {
            (Ticks::Trades(trades), Tick::Trade(t)) => trades.push(t),
            (Ticks::Quotes(quotes), Tick::Quote(q)) => quotes.push(q),
            _ => bail!("a file mixed different kinds of rows"),
        }
        self.held += 1;

        if self.newest.is_none_or(|newest| day > newest) {
            self.newest = Some(day);
            // Keep the day before too: rows around midnight are often a little out of order.
            let kept = self.days.split_off(&(day - 1));
            for (done, ticks) in std::mem::replace(&mut self.days, kept) {
                self.held -= ticks.len();
                writer.write_day(done, ticks)?;
            }
        }
        if self.held >= MAX_HELD {
            self.flush(writer)?;
        }
        Ok(())
    }

    fn flush(&mut self, writer: &mut MarketWriter) -> anyhow::Result<()> {
        for (day, ticks) in std::mem::take(&mut self.days) {
            writer.write_day(day, ticks)?;
        }
        self.held = 0;
        Ok(())
    }
}

// --- Profiles --------------------------------------------------------------------------------

/// Where saved formats live: `…/persephone/import-profiles`.
#[must_use]
pub fn profiles_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("persephone").join("import-profiles"))
}

/// Saves `format` as profile `name` in `dir`.
///
/// # Errors
///
/// Returns an error if the name isn't letters, digits, `-` and `_`, or the file can't be
/// written.
pub fn save_profile(dir: &Path, name: &str, format: &Format) -> anyhow::Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "profile names use letters, digits, `-` and `_`"
    );
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{name}.json"));
    fs::write(&path, serde_json::to_string_pretty(format)? + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Reads profile `name` from `dir`.
///
/// # Errors
///
/// Returns an error if there's no such profile or it's unreadable.
pub fn load_profile(dir: &Path, name: &str) -> anyhow::Result<Format> {
    let path = dir.join(format!("{name}.json"));
    let text = fs::read_to_string(&path).with_context(|| format!("there's no profile `{name}`"))?;
    serde_json::from_str(&text).with_context(|| format!("profile `{name}` is unreadable"))
}

#[cfg(test)]
mod tests;

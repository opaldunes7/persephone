//! The local market store: ticks imported from CSV files, kept as Parquet.
//!
//! ```text
//! store/<VENUE>/<SYMBOL>/
//!   market.json                         the instrument, its kind of ticks, fee and range
//!   ticks/<YYYY>/<YYYY-MM-DD>.parquet   one UTC day of ticks
//!   bars-1m/<YYYY-MM>.parquet           one month of 1-minute bars made from those ticks
//! ```
//!
//! Charts read the 1-minute bars, rolled up to their timeframe, so they never touch the ticks;
//! tick-level backtests stream the day files. Everything is plain Parquet (ZSTD), readable by
//! pandas, Polars or DuckDB.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail, ensure};
use arrow_array::{Array, ArrayRef, Float64Array, Int8Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::{Compression, Encoding, ZstdLevel},
    file::properties::WriterProperties,
    schema::types::ColumnPath,
};
use persephone_core::{
    Candle, InstrumentSpec, Timeframe, UnixNanos,
    tick::{self, Aggressor, Quote, TickFeed, TickKind, Ticks, Trade},
    time::{NANOS_PER_DAY, civil_from_days},
};
use serde::{Deserialize, Serialize};

const MARKET_FILE: &str = "market.json";

/// Every locally stored market, under one folder.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/Library/Application Support/persephone/store` on macOS, and the platform equivalent
    /// elsewhere.
    #[must_use]
    pub fn default_root() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("persephone").join("store"))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every market in the store, by venue then symbol. Folders without a readable
    /// `market.json` are skipped.
    #[must_use]
    pub fn markets(&self) -> Vec<LocalMarket> {
        let mut markets: Vec<LocalMarket> = subdirs(&self.root)
            .iter()
            .flat_map(|venue| subdirs(venue))
            .filter_map(|dir| LocalMarket::open(dir).ok())
            .collect();
        markets
            .sort_by(|a, b| (&a.info.venue, &a.info.symbol).cmp(&(&b.info.venue, &b.info.symbol)));
        markets
    }

    /// # Errors
    ///
    /// Returns an error if there's no such market or its `market.json` is unreadable.
    pub fn market(&self, venue: &str, symbol: &str) -> anyhow::Result<LocalMarket> {
        LocalMarket::open(self.dir(venue, symbol))
            .with_context(|| format!("there's no local market {symbol}.{venue}"))
    }

    /// Opens the market `info` describes, creating it if it doesn't exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the names aren't usable as folder names or the folder can't be
    /// written.
    pub fn create(&self, info: MarketInfo) -> anyhow::Result<LocalMarket> {
        check_name("symbol", &info.symbol)?;
        check_name("venue", &info.venue)?;
        let dir = self.dir(&info.venue, &info.symbol);
        if dir.join(MARKET_FILE).exists() {
            return LocalMarket::open(dir);
        }
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let market = LocalMarket { dir, info };
        market.save_info()?;
        Ok(market)
    }

    /// Deletes a market and everything stored for it.
    ///
    /// # Errors
    ///
    /// Returns an error if there's no such market or it can't be deleted.
    pub fn remove(&self, venue: &str, symbol: &str) -> anyhow::Result<()> {
        let market = self.market(venue, symbol)?;
        fs::remove_dir_all(&market.dir).with_context(|| format!("deleting {}", market.id()))
    }

    fn dir(&self, venue: &str, symbol: &str) -> PathBuf {
        self.root.join(venue).join(symbol)
    }
}

/// Symbols and venues name folders: uppercase letters, digits, `-` and `_`.
///
/// # Errors
///
/// Returns a message naming what's wrong.
pub fn check_name(what: &str, name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_');
    ensure!(
        ok,
        "the {what} `{name}` should be uppercase letters, digits, `-` or `_`"
    );
    Ok(())
}

/// What a stored market is, saved as its `market.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketInfo {
    pub symbol: String,
    pub venue: String,
    pub base: String,
    pub quote: String,
    pub price_precision: u8,
    pub size_precision: u8,
    #[serde(with = "kind_name")]
    pub kind: TickKind,
    /// Fee rate charged on every backtest fill (0.001 = 0.1%).
    pub fee: f64,
    /// First and last tick timestamps and the number of ticks; zero while empty.
    #[serde(default)]
    pub first: UnixNanos,
    #[serde(default)]
    pub last: UnixNanos,
    #[serde(default)]
    pub count: u64,
}

pub(crate) mod kind_name {
    use persephone_core::tick::TickKind;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(kind: &TickKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(kind.label())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TickKind, D::Error> {
        match String::deserialize(d)?.as_str() {
            "trades" => Ok(TickKind::Trades),
            "quotes" => Ok(TickKind::Quotes),
            other => Err(D::Error::custom(format!("unknown tick kind `{other}`"))),
        }
    }
}

/// One stored market.
#[derive(Clone, Debug)]
pub struct LocalMarket {
    dir: PathBuf,
    pub info: MarketInfo,
}

impl LocalMarket {
    fn open(dir: PathBuf) -> anyhow::Result<Self> {
        let path = dir.join(MARKET_FILE);
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let info = serde_json::from_str(&text).with_context(|| format!("{}", path.display()))?;
        Ok(Self { dir, info })
    }

    /// Writes [`info`](Self::info) back to `market.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be written.
    pub fn save_info(&self) -> anyhow::Result<()> {
        let path = self.dir.join(MARKET_FILE);
        let text = serde_json::to_string_pretty(&self.info)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text + "\n").with_context(|| format!("writing {}", path.display()))?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `SYMBOL.VENUE`, the way Nautilus names instruments.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}.{}", self.info.symbol, self.info.venue)
    }

    #[must_use]
    pub fn instrument(&self) -> InstrumentSpec {
        InstrumentSpec {
            symbol: self.info.symbol.clone(),
            venue: self.info.venue.clone(),
            base: self.info.base.clone(),
            quote: self.info.quote.clone(),
            price_precision: self.info.price_precision,
            size_precision: self.info.size_precision,
        }
    }

    /// The latest `max` bars of `timeframe` (fewer if there aren't that many), oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error if a bar file can't be read.
    pub fn candles(&self, timeframe: Timeframe, max: usize) -> anyhow::Result<Vec<Candle>> {
        let mut months = Vec::new();
        let mut total = 0;
        // Months start at midnight UTC and every timeframe divides a day, so each month
        // rolls up on its own.
        for path in self.bar_months().iter().rev() {
            let bars = tick::resample(&read_bars(path)?, timeframe);
            total += bars.len();
            months.push(bars);
            if total >= max {
                break;
            }
        }
        let mut candles: Vec<Candle> = months.into_iter().rev().flatten().collect();
        let excess = candles.len().saturating_sub(max);
        candles.drain(..excess);
        Ok(candles)
    }

    /// Every tick with `start <= ts < end`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error if a tick file can't be read.
    pub fn read_ticks(&self, start: UnixNanos, end: UnixNanos) -> anyhow::Result<Ticks> {
        let mut out = Ticks::empty(self.info.kind);
        if start >= end {
            return Ok(out);
        }
        for day in day_of(start)..=day_of(end - 1) {
            let path = self.day_path(day);
            if path.exists() {
                append_between(&mut out, read_ticks(&path, self.info.kind)?, start, end);
            }
        }
        Ok(out)
    }

    /// The UTC days (since 1970-01-01) that have ticks, in order.
    #[must_use]
    pub fn days(&self) -> Vec<i64> {
        let mut days: Vec<i64> = subdirs(&self.dir.join("ticks"))
            .iter()
            .flat_map(|year| parquet_files(year))
            .filter_map(|path| parse_day(path.file_stem()?.to_str()?))
            .collect();
        days.sort_unstable();
        days
    }

    fn day_path(&self, day: i64) -> PathBuf {
        let (y, m, d) = civil_from_days(day);
        self.dir
            .join("ticks")
            .join(format!("{y:04}"))
            .join(format!("{y:04}-{m:02}-{d:02}.parquet"))
    }

    fn month_path(&self, year: i64, month: u32) -> PathBuf {
        self.dir
            .join("bars-1m")
            .join(format!("{year:04}-{month:02}.parquet"))
    }

    fn bar_months(&self) -> Vec<PathBuf> {
        let mut months = parquet_files(&self.dir.join("bars-1m"));
        months.sort();
        months
    }
}

impl TickFeed for LocalMarket {
    fn ticks(&self, start: UnixNanos, end: UnixNanos) -> Result<Ticks, String> {
        self.read_ticks(start, end).map_err(|e| format!("{e:#}"))
    }
}

/// Adds ticks to a market, one UTC day at a time. The 1-minute bars and the market's summary
/// catch up in [`finish`](Self::finish).
#[derive(Debug)]
pub struct MarketWriter {
    market: LocalMarket,
    /// Days already written by this writer: more ticks for them are added, not swapped in.
    written: HashSet<i64>,
    /// 1-minute bars of the days written, by month, waiting for `finish`.
    bars: BTreeMap<(i64, u32), BTreeMap<i64, Vec<Candle>>>,
}

impl MarketWriter {
    #[must_use]
    pub fn new(market: LocalMarket) -> Self {
        Self {
            market,
            written: HashSet::new(),
            bars: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn market(&self) -> &LocalMarket {
        &self.market
    }

    /// Stores ticks from UTC `day` (in any order). Stored ticks inside the time span the new
    /// ones cover are replaced, so importing the same file twice doesn't double it, while
    /// files that each hold part of a day add up.
    ///
    /// # Errors
    ///
    /// Returns an error if the ticks are the wrong kind or fall outside `day`, or a file
    /// can't be read or written.
    pub fn write_day(&mut self, day: i64, mut ticks: Ticks) -> anyhow::Result<()> {
        let kind = self.market.info.kind;
        ensure!(
            ticks.kind() == kind,
            "{} holds {kind}, not {}",
            self.market.id(),
            ticks.kind()
        );
        if ticks.is_empty() {
            return Ok(());
        }
        sort(&mut ticks);
        ensure!(
            day_of(ticks.ts(0)) == day && day_of(ticks.ts(ticks.len() - 1)) == day,
            "ticks handed over for the wrong day"
        );

        let path = self.market.day_path(day);
        let ticks = if path.exists() {
            let stored = read_ticks(&path, kind)?;
            merge(stored, ticks, !self.written.contains(&day))
        } else {
            ticks
        };
        write_ticks(&path, &ticks)?;

        let (year, month, _) = civil_from_days(day);
        self.bars
            .entry((year, month))
            .or_default()
            .insert(day, tick::bars(&ticks, Timeframe::M1));
        self.written.insert(day);
        Ok(())
    }

    /// Writes the 1-minute bars of every day written and brings `market.json` up to date.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read or written.
    pub fn finish(mut self) -> anyhow::Result<LocalMarket> {
        for ((year, month), days) in std::mem::take(&mut self.bars) {
            let path = self.market.month_path(year, month);
            let mut bars = if path.exists() {
                read_bars(&path)?
            } else {
                Vec::new()
            };
            bars.retain(|b| !days.contains_key(&day_of(b.ts)));
            bars.extend(days.into_values().flatten());
            bars.sort_by_key(|b| b.ts);
            write_bars(&path, &bars)?;
        }

        let market = &mut self.market;
        let days = market.days();
        let mut count = 0;
        for &day in &days {
            count += row_count(&market.day_path(day))?;
        }
        let ends = |day: Option<&i64>, last: bool| -> anyhow::Result<UnixNanos> {
            let Some(&day) = day else { return Ok(0) };
            let ticks = read_ticks(&market.day_path(day), market.info.kind)?;
            Ok(if last {
                ticks.ts(ticks.len() - 1)
            } else {
                ticks.ts(0)
            })
        };
        let (first, last) = (ends(days.first(), false)?, ends(days.last(), true)?);
        market.info.first = first;
        market.info.last = last;
        market.info.count = count;
        market.save_info()?;
        Ok(self.market)
    }
}

/// The UTC day (since 1970-01-01) a timestamp falls on.
#[must_use]
pub fn day_of(ts: UnixNanos) -> i64 {
    (ts / NANOS_PER_DAY) as i64
}

fn parse_day(stem: &str) -> Option<i64> {
    let mut parts = stem.split('-').map(str::parse::<u32>);
    let (y, m, d) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    Some(persephone_core::time::days_from_civil(i64::from(y), m, d))
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect()
}

fn parquet_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "parquet"))
        .collect()
}

fn sort(ticks: &mut Ticks) {
    // Stable, so ticks sharing a timestamp keep their file order.
    match ticks {
        Ticks::Trades(t) => t.sort_by_key(|t| t.ts),
        Ticks::Quotes(q) => q.sort_by_key(|q| q.ts),
    }
}

/// Adds `new` to `stored`, first dropping stored ticks inside `new`'s span if `replace_span`.
fn merge(stored: Ticks, new: Ticks, replace_span: bool) -> Ticks {
    fn merge<T>(mut stored: Vec<T>, new: Vec<T>, replace_span: bool, ts: fn(&T) -> u64) -> Vec<T> {
        if replace_span && let (Some(first), Some(last)) = (new.first(), new.last()) {
            let (from, to) = (ts(first), ts(last));
            stored.retain(|t| ts(t) < from || ts(t) > to);
        }
        stored.extend(new);
        stored.sort_by_key(ts);
        stored
    }
    match (stored, new) {
        (Ticks::Trades(s), Ticks::Trades(n)) => Ticks::Trades(merge(s, n, replace_span, |t| t.ts)),
        (Ticks::Quotes(s), Ticks::Quotes(n)) => Ticks::Quotes(merge(s, n, replace_span, |q| q.ts)),
        (stored, _) => stored,
    }
}

fn append_between(out: &mut Ticks, ticks: Ticks, start: UnixNanos, end: UnixNanos) {
    let within = |ts: UnixNanos| ts >= start && ts < end;
    match (out, ticks) {
        (Ticks::Trades(out), Ticks::Trades(t)) => {
            out.extend(t.into_iter().filter(|t| within(t.ts)))
        }
        (Ticks::Quotes(out), Ticks::Quotes(q)) => {
            out.extend(q.into_iter().filter(|q| within(q.ts)))
        }
        _ => {}
    }
}

// --- Parquet files ---------------------------------------------------------------------------

fn write_ticks(path: &Path, ticks: &Ticks) -> anyhow::Result<()> {
    let floats = |values: Vec<f64>| -> ArrayRef { Arc::new(Float64Array::from(values)) };
    let (names, columns): (Vec<&str>, Vec<ArrayRef>) = match ticks {
        Ticks::Trades(t) => (
            vec!["ts", "price", "size", "aggressor"],
            vec![
                timestamps(t.iter().map(|t| t.ts)),
                floats(t.iter().map(|t| t.price).collect()),
                floats(t.iter().map(|t| t.size).collect()),
                Arc::new(Int8Array::from_iter_values(t.iter().map(
                    |t| match t.aggressor {
                        Aggressor::Buyer => 1,
                        Aggressor::Seller => -1,
                        Aggressor::Unknown => 0,
                    },
                ))),
            ],
        ),
        Ticks::Quotes(q) => (
            vec!["ts", "bid", "ask", "bid_size", "ask_size"],
            vec![
                timestamps(q.iter().map(|q| q.ts)),
                floats(q.iter().map(|q| q.bid).collect()),
                floats(q.iter().map(|q| q.ask).collect()),
                floats(q.iter().map(|q| q.bid_size).collect()),
                floats(q.iter().map(|q| q.ask_size).collect()),
            ],
        ),
    };
    write_columns(path, &names, columns)
}

fn read_ticks(path: &Path, kind: TickKind) -> anyhow::Result<Ticks> {
    let mut ticks = Ticks::empty(kind);
    for batch in read_batches(path)? {
        let ts = column::<Int64Array>(&batch, "ts")?.values();
        match &mut ticks {
            Ticks::Trades(out) => {
                let price = column::<Float64Array>(&batch, "price")?.values();
                let size = column::<Float64Array>(&batch, "size")?.values();
                let aggressor = column::<Int8Array>(&batch, "aggressor")?.values();
                out.extend((0..batch.num_rows()).map(|i| Trade {
                    ts: ts[i] as UnixNanos,
                    price: price[i],
                    size: size[i],
                    aggressor: match aggressor[i] {
                        1 => Aggressor::Buyer,
                        -1 => Aggressor::Seller,
                        _ => Aggressor::Unknown,
                    },
                }));
            }
            Ticks::Quotes(out) => {
                let bid = column::<Float64Array>(&batch, "bid")?.values();
                let ask = column::<Float64Array>(&batch, "ask")?.values();
                let bid_size = column::<Float64Array>(&batch, "bid_size")?.values();
                let ask_size = column::<Float64Array>(&batch, "ask_size")?.values();
                out.extend((0..batch.num_rows()).map(|i| Quote {
                    ts: ts[i] as UnixNanos,
                    bid: bid[i],
                    ask: ask[i],
                    bid_size: bid_size[i],
                    ask_size: ask_size[i],
                }));
            }
        }
    }
    Ok(ticks)
}

fn write_bars(path: &Path, bars: &[Candle]) -> anyhow::Result<()> {
    let floats = |f: fn(&Candle) -> f64| -> ArrayRef {
        Arc::new(Float64Array::from_iter_values(bars.iter().map(f)))
    };
    write_columns(
        path,
        &["ts", "open", "high", "low", "close", "volume"],
        vec![
            timestamps(bars.iter().map(|b| b.ts)),
            floats(|b| b.open),
            floats(|b| b.high),
            floats(|b| b.low),
            floats(|b| b.close),
            floats(|b| b.volume),
        ],
    )
}

fn read_bars(path: &Path) -> anyhow::Result<Vec<Candle>> {
    let mut bars = Vec::new();
    for batch in read_batches(path)? {
        let ts = column::<Int64Array>(&batch, "ts")?.values();
        let col = |name| column::<Float64Array>(&batch, name).map(|c| c.values());
        let (open, high, low, close, volume) = (
            col("open")?,
            col("high")?,
            col("low")?,
            col("close")?,
            col("volume")?,
        );
        bars.extend((0..batch.num_rows()).map(|i| Candle {
            ts: ts[i] as UnixNanos,
            open: open[i],
            high: high[i],
            low: low[i],
            close: close[i],
            volume: volume[i],
        }));
    }
    Ok(bars)
}

fn timestamps(ts: impl Iterator<Item = UnixNanos>) -> ArrayRef {
    Arc::new(Int64Array::from_iter_values(ts.map(|ts| ts as i64)))
}

/// Writes columns (`ts` first, then floats and small integers) to `path`, atomically.
fn write_columns(path: &Path, names: &[&str], columns: Vec<ArrayRef>) -> anyhow::Result<()> {
    let fields: Vec<Field> = names
        .iter()
        .zip(&columns)
        .map(|(name, column)| Field::new(*name, column.data_type().clone(), false))
        .collect();
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)?;

    // Sorted timestamps delta-encode to almost nothing; splitting floats by byte helps ZSTD.
    let mut properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_dictionary_enabled(false)
        .set_column_encoding(ColumnPath::from("ts"), Encoding::DELTA_BINARY_PACKED);
    for (name, column) in names.iter().zip(batch.columns()) {
        if column.data_type() == &DataType::Float64 {
            properties = properties
                .set_column_encoding(ColumnPath::from(*name), Encoding::BYTE_STREAM_SPLIT);
        }
    }

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("parquet.tmp");
    let file = fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(properties.build()))?;
    writer.write(&batch)?;
    writer.close()?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn read_batches(path: &Path) -> anyhow::Result<Vec<RecordBatch>> {
    let file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("reading {}", path.display()))?
        .with_batch_size(64 * 1024)
        .build()?;
    let batches: Result<Vec<_>, _> = reader.collect();
    batches.with_context(|| format!("reading {}", path.display()))
}

fn row_count(path: &Path) -> anyhow::Result<u64> {
    let file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    Ok(builder.metadata().file_metadata().num_rows().try_into()?)
}

fn column<'a, T: Array + 'static>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a T> {
    let Some(column) = batch.column_by_name(name) else {
        bail!("missing column `{name}`");
    };
    column
        .as_any()
        .downcast_ref::<T>()
        .with_context(|| format!("column `{name}` has the wrong type"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: UnixNanos = NANOS_PER_DAY;
    const MIN: UnixNanos = 60_000_000_000;

    fn temp_store(name: &str) -> Store {
        let root =
            std::env::temp_dir().join(format!("persephone-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Store::new(root)
    }

    fn info(kind: TickKind) -> MarketInfo {
        MarketInfo {
            symbol: "EURUSD".into(),
            venue: "LOCAL".into(),
            base: "EUR".into(),
            quote: "USD".into(),
            price_precision: 5,
            size_precision: 0,
            kind,
            fee: 0.0,
            first: 0,
            last: 0,
            count: 0,
        }
    }

    fn trades(day: i64, minutes: impl Iterator<Item = u64>, price: f64) -> Ticks {
        Ticks::Trades(
            minutes
                .map(|m| Trade {
                    ts: day as u64 * DAY + m * MIN + 1,
                    price: price + m as f64,
                    size: 1.0,
                    aggressor: Aggressor::Buyer,
                })
                .collect(),
        )
    }

    #[test]
    fn ticks_round_trip_and_bars_are_ready_for_charts() {
        let store = temp_store("round-trip");
        let market = store.create(info(TickKind::Trades)).unwrap();
        let mut writer = MarketWriter::new(market);
        writer
            .write_day(20_000, trades(20_000, 0..1_440, 1.0))
            .unwrap();
        writer
            .write_day(20_001, trades(20_001, 0..60, 2.0))
            .unwrap();
        let market = writer.finish().unwrap();

        assert_eq!(market.info.count, 1_500);
        assert_eq!(market.info.first, 20_000 * DAY + 1);
        assert_eq!(market.days(), vec![20_000, 20_001]);

        let all = market.read_ticks(0, u64::MAX).unwrap();
        assert_eq!(all, {
            let (Ticks::Trades(mut a), Ticks::Trades(b)) =
                (trades(20_000, 0..1_440, 1.0), trades(20_001, 0..60, 2.0))
            else {
                unreachable!()
            };
            a.extend(b);
            Ticks::Trades(a)
        });
        assert_eq!(
            market
                .read_ticks(20_000 * DAY, 20_000 * DAY + 2 * MIN)
                .unwrap()
                .len(),
            2
        );

        let hourly = market.candles(Timeframe::H1, 3).unwrap();
        assert_eq!(hourly.len(), 3, "the latest three hours");
        assert_eq!(hourly[2].ts, 20_001 * DAY);
        assert_eq!(hourly[2].volume, 60.0);
        assert_eq!(market.candles(Timeframe::D1, 100).unwrap().len(), 2);

        let reopened = store.market("LOCAL", "EURUSD").unwrap();
        assert_eq!(reopened.info, market.info);
        assert_eq!(store.markets().len(), 1);
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn reimporting_replaces_and_adjacent_files_add_up() {
        let store = temp_store("merge");
        let write = |ticks: Ticks| {
            let market = store.market("LOCAL", "EURUSD").unwrap();
            let mut writer = MarketWriter::new(market);
            writer.write_day(20_000, ticks).unwrap();
            writer.finish().unwrap()
        };
        store.create(info(TickKind::Trades)).unwrap();

        write(trades(20_000, 0..600, 1.0));
        assert_eq!(
            write(trades(20_000, 0..600, 1.0)).info.count,
            600,
            "same file twice"
        );
        assert_eq!(
            write(trades(20_000, 600..1_440, 1.0)).info.count,
            1_440,
            "the rest of the day"
        );
        let market = write(trades(20_000, 100..200, 5.0));
        assert_eq!(
            market.info.count, 1_440,
            "a corrected stretch replaces its span"
        );
        let Ticks::Trades(t) = market.read_ticks(0, u64::MAX).unwrap() else {
            unreachable!()
        };
        assert_eq!(t[150].price, 155.0);
        assert_eq!(t[250].price, 251.0);

        // Within one writer, a day handed over twice is added up (unsorted input).
        let mut writer = MarketWriter::new(market);
        writer
            .write_day(20_000, trades(20_000, 0..10, 9.0))
            .unwrap();
        writer
            .write_day(20_000, trades(20_000, 0..10, 9.0))
            .unwrap();
        assert_eq!(writer.finish().unwrap().info.count, 1_440 + 10);
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn markets_hold_one_kind_and_sane_names() {
        let store = temp_store("kinds");
        let market = store.create(info(TickKind::Quotes)).unwrap();
        let mut writer = MarketWriter::new(market);
        assert!(writer.write_day(20_000, trades(20_000, 0..1, 1.0)).is_err());
        let bad = MarketInfo {
            symbol: "EUR/USD".into(),
            ..info(TickKind::Quotes)
        };
        assert!(store.create(bad).is_err());
        fs::remove_dir_all(store.root()).unwrap();
    }
}

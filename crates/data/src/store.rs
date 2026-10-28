//! The local market store: ticks or bars imported from CSV files, kept as Parquet.
//!
//! ```text
//! store/<VENUE>/<SYMBOL>/
//!   market.json                         the instrument, what it holds, fee and range
//!   ticks/<YYYY>/<YYYY-MM-DD>.parquet   one UTC day of ticks
//!   bars-1m/<YYYY-MM>.parquet           one month of 1-minute bars made from those ticks
//!   bars/<YYYY>.parquet                 or, for a market imported as bars, one year of them
//! ```
//!
//! Charts read the bars, rolled up to their timeframe, so they never touch the ticks; tick-level
//! backtests stream the day files. Everything is plain Parquet (ZSTD), readable by pandas,
//! Polars or DuckDB.

use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
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
    BarSpec, Candle, InstrumentSpec, Timeframe, UnixNanos,
    tick::{self, Aggressor, Quote, TickFeed, TickKind, Ticks, Trade},
    time::{NANOS_PER_DAY, NANOS_PER_MINUTE, civil_from_days},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    /// Saved as `"kind": "trades"` or `"quotes"`, or `"bars"` with a `"spacing"`.
    #[serde(flatten, with = "market_kind")]
    pub kind: MarketKind,
    /// Fee rate charged on every backtest fill (0.001 = 0.1%).
    pub fee: f64,
    /// First and last timestamps (of ticks, or of bar opens) and how many there are; zero
    /// while empty.
    #[serde(default)]
    pub first: UnixNanos,
    #[serde(default)]
    pub last: UnixNanos,
    #[serde(default)]
    pub count: u64,
}

/// What a market holds: ticks of one kind, or bars imported as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketKind {
    Ticks(TickKind),
    Bars(Spacing),
}

impl MarketKind {
    #[must_use]
    pub const fn ticks(self) -> Option<TickKind> {
        match self {
            Self::Ticks(kind) => Some(kind),
            Self::Bars(_) => None,
        }
    }

    #[must_use]
    pub const fn spacing(self) -> Option<Spacing> {
        match self {
            Self::Ticks(_) => None,
            Self::Bars(spacing) => Some(spacing),
        }
    }

    /// Whether charts can show this market's bars at `timeframe`.
    #[must_use]
    pub fn allows(self, timeframe: Timeframe) -> bool {
        self.spacing()
            .is_none_or(|spacing| spacing.allows(timeframe))
    }
}

impl fmt::Display for MarketKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ticks(kind) => write!(f, "{kind}"),
            Self::Bars(spacing) => write!(f, "{spacing} bars"),
        }
    }
}

mod market_kind {
    use persephone_core::tick::TickKind;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    use super::{MarketKind, Spacing};

    #[derive(Serialize, Deserialize)]
    struct Fields {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spacing: Option<Spacing>,
    }

    pub fn serialize<S: Serializer>(kind: &MarketKind, s: S) -> Result<S::Ok, S::Error> {
        let (kind, spacing) = match kind {
            MarketKind::Ticks(kind) => (kind.label(), None),
            MarketKind::Bars(spacing) => ("bars", Some(*spacing)),
        };
        Fields {
            kind: kind.to_owned(),
            spacing,
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<MarketKind, D::Error> {
        let fields = Fields::deserialize(d)?;
        match fields.kind.as_str() {
            "trades" => Ok(MarketKind::Ticks(TickKind::Trades)),
            "quotes" => Ok(MarketKind::Ticks(TickKind::Quotes)),
            "bars" => fields
                .spacing
                .map(MarketKind::Bars)
                .ok_or_else(|| D::Error::custom("bars need a `spacing`")),
            other => Err(D::Error::custom(format!("unknown kind `{other}`"))),
        }
    }
}

/// How far apart a bar market's bars are: a number of minutes that divides a day, a week, or
/// a month. Written like a timeframe: `3m`, `2h`, `1d`, `1w`, `1M`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spacing {
    Minutes(u32),
    Week,
    Month,
}

impl Spacing {
    const DAY_MINUTES: u32 = 1_440;

    /// The spacing of bars that open `gap` apart, and whether they're closer than a minute
    /// (and so get rolled up into 1-minute bars). Gaps that don't divide a day round down to
    /// one that does (7 minutes → 1 minute, 2 days → 1 day).
    #[must_use]
    pub fn from_gap(gap: UnixNanos) -> (Self, bool) {
        const DAY: UnixNanos = NANOS_PER_DAY;
        if gap > 26 * DAY {
            return (Self::Month, false);
        }
        if gap >= 6 * DAY {
            return (Self::Week, false);
        }
        let minutes = (gap + NANOS_PER_MINUTE / 2) / NANOS_PER_MINUTE;
        if minutes == 0 {
            return (Self::Minutes(1), true);
        }
        let minutes = u32::try_from(minutes).unwrap_or(u32::MAX);
        (Self::Minutes(gcd(minutes, Self::DAY_MINUTES)), false)
    }

    /// The spacing of bars opening at `times` (in file order), from the median gap; `None`
    /// with fewer than two distinct times.
    #[must_use]
    pub fn from_times(times: &[UnixNanos]) -> Option<(Self, bool)> {
        let mut gaps: Vec<UnixNanos> = times
            .windows(2)
            .filter(|w| w[1] > w[0])
            .map(|w| w[1] - w[0])
            .collect();
        if gaps.is_empty() {
            return None;
        }
        let middle = gaps.len() / 2;
        Some(Self::from_gap(*gaps.select_nth_unstable(middle).1))
    }

    /// When the bar holding `ts` opens.
    #[must_use]
    pub fn open_time(self, ts: UnixNanos) -> UnixNanos {
        match self {
            Self::Minutes(m) => ts - ts % (UnixNanos::from(m) * NANOS_PER_MINUTE),
            Self::Week => Timeframe::W1.open_time(ts),
            Self::Month => Timeframe::MN1.open_time(ts),
        }
    }

    /// Whether `timeframe`'s bars are made of whole bars of this spacing.
    #[must_use]
    pub fn allows(self, timeframe: Timeframe) -> bool {
        match (self, timeframe) {
            (Self::Week, tf) => tf == Timeframe::W1,
            (Self::Month, tf) => tf == Timeframe::MN1,
            (Self::Minutes(_), Timeframe::W1 | Timeframe::MN1) => true,
            (Self::Minutes(m), tf) => tf.nanos() % (UnixNanos::from(m) * NANOS_PER_MINUTE) == 0,
        }
    }

    /// The shortest timeframe charts can show these bars at.
    #[must_use]
    pub fn shortest(self) -> Timeframe {
        Timeframe::ALL
            .into_iter()
            .find(|&tf| self.allows(tf))
            .unwrap_or(Timeframe::MN1)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl fmt::Display for Spacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Minutes(m) if m % Self::DAY_MINUTES == 0 => {
                write!(f, "{}d", m / Self::DAY_MINUTES)
            }
            Self::Minutes(m) if m % 60 == 0 => write!(f, "{}h", m / 60),
            Self::Minutes(m) => write!(f, "{m}m"),
            Self::Week => f.write_str("1w"),
            Self::Month => f.write_str("1M"),
        }
    }
}

impl FromStr for Spacing {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "1w" => return Ok(Self::Week),
            "1M" => return Ok(Self::Month),
            _ => {}
        }
        let unit = s.len().checked_sub(1).and_then(|at| s.get(at..));
        let count = s
            .get(..s.len().saturating_sub(1))
            .and_then(|n| n.parse::<u32>().ok());
        let minutes = match (count, unit) {
            (Some(n), Some("m")) => Some(n),
            (Some(n), Some("h")) => n.checked_mul(60),
            (Some(n), Some("d")) => n.checked_mul(Self::DAY_MINUTES),
            _ => None,
        };
        match minutes {
            Some(m) if m > 0 && Self::DAY_MINUTES % m == 0 => Ok(Self::Minutes(m)),
            _ => bail!(
                "`{s}` isn't a bar spacing (minutes, hours or a day that divide a day, 1w or 1M)"
            ),
        }
    }
}

impl Serialize for Spacing {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Spacing {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
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
    /// Returns an error if a bar file can't be read, or the market's bars are too far apart
    /// for `timeframe`.
    pub fn candles(&self, timeframe: Timeframe, max: usize) -> anyhow::Result<Vec<Candle>> {
        let files = self.bar_files(timeframe)?;
        let mut chunks = Vec::new();
        let mut candles = Vec::new();
        for path in files.iter().rev() {
            chunks.push(roll_up_file(path, timeframe)?);
            candles = tick::resample(
                &chunks.iter().rev().flatten().copied().collect::<Vec<_>>(),
                timeframe,
            );
            // One more than wanted, as the oldest can lack its part in an earlier file.
            if candles.len() > max {
                break;
            }
        }
        let excess = candles.len().saturating_sub(max);
        candles.drain(..excess);
        Ok(candles)
    }

    /// The bars of `spec` that open in `[from, to)`, oldest first. Time bars come from the
    /// stored bars; tick and volume bars from the ticks in the range, counted from its first.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read, the market's bars are too far apart for the
    /// timeframe, or tick or volume bars are asked of a market that has no ticks.
    pub fn candles_between(
        &self,
        spec: BarSpec,
        from: UnixNanos,
        to: UnixNanos,
    ) -> anyhow::Result<Vec<Candle>> {
        let BarSpec::Time(timeframe) = spec else {
            ensure!(
                self.info.kind.ticks().is_some(),
                "{} was imported as bars, so it has no ticks to make tick or volume bars from",
                self.id()
            );
            return Ok(tick::bars_by(&self.read_ticks(from, to)?, spec));
        };
        let files = self.bar_files(timeframe)?;
        if from >= to {
            return Ok(Vec::new());
        }
        // The last bar may close past `to`, so read its whole period.
        let until = timeframe.close_time(to - 1);
        let mut finer = Vec::new();
        for path in files {
            if file_span(&path).is_some_and(|(start, end)| end > from && start < until) {
                finer.extend(roll_up_file(&path, timeframe)?);
            }
        }
        let mut candles = tick::resample(&finer, timeframe);
        candles.retain(|c| c.ts >= from && c.ts < to);
        Ok(candles)
    }

    /// The first and last timestamps it holds (of ticks, or of bar opens); zero while empty.
    #[must_use]
    pub fn span(&self) -> (UnixNanos, UnixNanos) {
        (self.info.first, self.info.last)
    }

    /// The files of stored bars, oldest first, after checking they make `timeframe`.
    fn bar_files(&self, timeframe: Timeframe) -> anyhow::Result<Vec<PathBuf>> {
        Ok(match self.info.kind {
            MarketKind::Ticks(_) => self.bar_months(),
            MarketKind::Bars(spacing) => {
                ensure!(
                    spacing.allows(timeframe),
                    "{} has {spacing} bars, which don't make {timeframe} bars",
                    self.id()
                );
                self.bar_years()
            }
        })
    }

    /// The latest `max` bars of `spec`. Time bars come from the stored bars; tick and volume
    /// bars from the ticks of the latest days (as many as it takes), counted from the first
    /// tick of the earliest day read.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read, or tick or volume bars are asked of a market
    /// that has no ticks.
    pub fn candles_by(&self, spec: BarSpec, max: usize) -> anyhow::Result<Vec<Candle>> {
        if let BarSpec::Time(timeframe) = spec {
            return self.candles(timeframe, max);
        }
        ensure!(
            self.info.kind.ticks().is_some(),
            "{} was imported as bars, so it has no ticks to make tick or volume bars from",
            self.id()
        );
        let days = self.days();
        let mut take = 1;
        loop {
            let Some(&first) = days.get(days.len().saturating_sub(take)) else {
                return Ok(Vec::new());
            };
            let ticks = self.read_ticks(first as u64 * NANOS_PER_DAY, self.info.last + 1)?;
            let mut bars = tick::bars_by(&ticks, spec);
            if bars.len() >= max || take >= days.len() {
                let excess = bars.len().saturating_sub(max);
                bars.drain(..excess);
                return Ok(bars);
            }
            // Read more days, doubling, so the total work stays within twice the last read.
            take = (take * 2).min(days.len());
        }
    }

    /// Every tick with `start <= ts < end`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error if a tick file can't be read, or the market holds bars.
    pub fn read_ticks(&self, start: UnixNanos, end: UnixNanos) -> anyhow::Result<Ticks> {
        let kind = self.tick_kind()?;
        let mut out = Ticks::empty(kind);
        if start >= end {
            return Ok(out);
        }
        for day in day_of(start)..=day_of(end - 1) {
            let path = self.day_path(day);
            if path.exists() {
                append_between(&mut out, read_ticks(&path, kind)?, start, end);
            }
        }
        Ok(out)
    }

    fn tick_kind(&self) -> anyhow::Result<TickKind> {
        self.info
            .kind
            .ticks()
            .with_context(|| format!("{} holds {}, not ticks", self.id(), self.info.kind))
    }

    /// How many bytes its files take on disk.
    #[must_use]
    pub fn disk_size(&self) -> u64 {
        fn size(path: &Path) -> u64 {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => fs::read_dir(path)
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|e| size(&e.path()))
                            .sum()
                    })
                    .unwrap_or(0),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            }
        }
        size(&self.dir)
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

    fn year_path(&self, year: i64) -> PathBuf {
        self.dir.join("bars").join(format!("{year:04}.parquet"))
    }

    fn bar_years(&self) -> Vec<PathBuf> {
        let mut years = parquet_files(&self.dir.join("bars"));
        years.sort();
        years
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
            kind == MarketKind::Ticks(ticks.kind()),
            "{} holds {kind}, not {}",
            self.market.id(),
            ticks.kind()
        );
        let kind = ticks.kind();
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
        let kind = market.tick_kind()?;
        let days = market.days();
        let mut count = 0;
        for &day in &days {
            count += row_count(&market.day_path(day))?;
        }
        let ends = |day: Option<&i64>, last: bool| -> anyhow::Result<UnixNanos> {
            let Some(&day) = day else { return Ok(0) };
            let ticks = read_ticks(&market.day_path(day), kind)?;
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

/// Adds bars to a market imported as bars, one UTC year at a time. The market's summary
/// catches up in [`finish`](Self::finish).
#[derive(Debug)]
pub struct BarWriter {
    market: LocalMarket,
    spacing: Spacing,
    /// Years already written by this writer: more bars for them are added, not swapped in.
    written: HashSet<i64>,
}

impl BarWriter {
    /// # Errors
    ///
    /// Returns an error if the market holds ticks.
    pub fn new(market: LocalMarket) -> anyhow::Result<Self> {
        let Some(spacing) = market.info.kind.spacing() else {
            bail!("{} holds {}, not bars", market.id(), market.info.kind);
        };
        Ok(Self {
            market,
            spacing,
            written: HashSet::new(),
        })
    }

    /// Stores bars from UTC `year` (in any order), each moved to its spacing's open time; bars
    /// that then share a time become one. Stored bars inside the time span the new ones cover
    /// are replaced, so importing the same file twice doesn't double it.
    ///
    /// # Errors
    ///
    /// Returns an error if the bars fall outside `year`, or a file can't be read or written.
    pub fn write_year(&mut self, year: i64, mut bars: Vec<Candle>) -> anyhow::Result<()> {
        if bars.is_empty() {
            return Ok(());
        }
        for bar in &mut bars {
            bar.ts = self.spacing.open_time(bar.ts);
        }
        bars.sort_by_key(|b| b.ts);
        ensure!(
            year_of(bars[0].ts) == year && year_of(bars[bars.len() - 1].ts) == year,
            "bars handed over for the wrong year"
        );
        let path = self.market.year_path(year);
        let bars = if path.exists() {
            merge_sorted(
                read_bars(&path)?,
                bars,
                !self.written.contains(&year),
                |b| b.ts,
            )
        } else {
            bars
        };
        write_bars(&path, &combine_same_times(bars))?;
        self.written.insert(year);
        Ok(())
    }

    /// Brings `market.json` up to date.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read or written.
    pub fn finish(mut self) -> anyhow::Result<LocalMarket> {
        let market = &mut self.market;
        let years = market.bar_years();
        let mut count = 0;
        for path in &years {
            count += row_count(path)?;
        }
        let ends = |path: Option<&PathBuf>, last: bool| -> anyhow::Result<UnixNanos> {
            let Some(path) = path else { return Ok(0) };
            let bars = read_bars(path)?;
            let bar = if last { bars.last() } else { bars.first() };
            Ok(bar.map_or(0, |b| b.ts))
        };
        market.info.first = ends(years.first(), false)?;
        market.info.last = ends(years.last(), true)?;
        market.info.count = count;
        market.save_info()?;
        Ok(self.market)
    }
}

/// Bars (oldest first) with any that open at the same time made into one.
fn combine_same_times(bars: Vec<Candle>) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::with_capacity(bars.len());
    for bar in bars {
        match out.last_mut() {
            Some(last) if last.ts == bar.ts => {
                last.high = last.high.max(bar.high);
                last.low = last.low.min(bar.low);
                last.close = bar.close;
                last.volume += bar.volume;
            }
            _ => out.push(bar),
        }
    }
    out
}

/// One file of stored bars rolled up as far toward `timeframe` as it can go on its own. Files
/// start at midnight UTC and timeframes up to a day divide a day, so those roll up fully;
/// weeks and months can straddle files, so they stop at days and roll up across files after.
fn roll_up_file(path: &Path, timeframe: Timeframe) -> anyhow::Result<Vec<Candle>> {
    let per_file = match timeframe {
        Timeframe::W1 | Timeframe::MN1 => Timeframe::D1,
        tf => tf,
    };
    Ok(tick::resample(&read_bars(path)?, per_file))
}

/// The period a bar file covers, from its name: `2024-03.parquet` a month, `2024.parquet` a
/// year.
fn file_span(path: &Path) -> Option<(UnixNanos, UnixNanos)> {
    let stem = path.file_stem()?.to_str()?;
    let start = |year: i64, month: u32| {
        persephone_core::time::days_from_civil(year, month, 1).max(0) as UnixNanos * NANOS_PER_DAY
    };
    match stem.split_once('-') {
        Some((year, month)) => {
            let (year, month): (i64, u32) = (year.parse().ok()?, month.parse().ok()?);
            let next = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            Some((start(year, month), start(next.0, next.1)))
        }
        None => {
            let year: i64 = stem.parse().ok()?;
            Some((start(year, 1), start(year + 1, 1)))
        }
    }
}

/// The UTC day (since 1970-01-01) a timestamp falls on.
#[must_use]
pub fn day_of(ts: UnixNanos) -> i64 {
    (ts / NANOS_PER_DAY) as i64
}

/// The UTC year a timestamp falls in.
#[must_use]
pub fn year_of(ts: UnixNanos) -> i64 {
    civil_from_days(day_of(ts)).0
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
    match (stored, new) {
        (Ticks::Trades(s), Ticks::Trades(n)) => {
            Ticks::Trades(merge_sorted(s, n, replace_span, |t| t.ts))
        }
        (Ticks::Quotes(s), Ticks::Quotes(n)) => {
            Ticks::Quotes(merge_sorted(s, n, replace_span, |q| q.ts))
        }
        (stored, _) => stored,
    }
}

/// Adds sorted `new` to `stored`, first dropping stored items inside `new`'s span if
/// `replace_span`.
fn merge_sorted<T>(
    mut stored: Vec<T>,
    new: Vec<T>,
    replace_span: bool,
    ts: fn(&T) -> u64,
) -> Vec<T> {
    if replace_span && let (Some(first), Some(last)) = (new.first(), new.last()) {
        let (from, to) = (ts(first), ts(last));
        stored.retain(|t| ts(t) < from || ts(t) > to);
    }
    stored.extend(new);
    stored.sort_by_key(ts);
    stored
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
            kind: MarketKind::Ticks(kind),
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
    fn tick_and_volume_bars_come_from_the_latest_ticks() {
        let store = temp_store("tick-bars");
        let market = store.create(info(TickKind::Trades)).unwrap();
        let mut writer = MarketWriter::new(market);
        for day in 20_000..20_010 {
            writer.write_day(day, trades(day, 0..1_440, 1.0)).unwrap();
        }
        let market = writer.finish().unwrap();

        let bars = market.candles_by(BarSpec::Ticks(100), 50).unwrap();
        assert_eq!(bars.len(), 50);
        // 50 bars take four days of ticks (5,760 from tick 8,640), so the last bar opens on tick
        // 8,640 + 5,700.
        assert_eq!(
            bars[49].ts,
            market.read_ticks(0, u64::MAX).unwrap().ts(14_340)
        );
        assert_eq!(
            market.candles_by(BarSpec::Ticks(100), 1_000).unwrap().len(),
            144,
            "every tick read"
        );
        // Each trade has size 1, so 60 traded makes a bar.
        let by_volume = market.candles_by(BarSpec::Volume(60.0), 10).unwrap();
        assert!(by_volume.iter().all(|b| b.volume == 60.0));
        assert_eq!(market.candles_by(Timeframe::H1.into(), 5).unwrap().len(), 5);
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

    fn bar(ts: UnixNanos, price: f64) -> Candle {
        Candle {
            ts,
            open: price,
            high: price + 1.0,
            low: price - 1.0,
            close: price,
            volume: 10.0,
        }
    }

    #[test]
    fn markets_read_old_and_new_kinds() {
        let old = r#"{"symbol":"EURUSD","venue":"LOCAL","base":"EUR","quote":"USD",
            "price_precision":5,"size_precision":0,"kind":"quotes","fee":0.0}"#;
        let info: MarketInfo = serde_json::from_str(old).unwrap();
        assert_eq!(info.kind, MarketKind::Ticks(TickKind::Quotes));
        let bars = MarketInfo {
            kind: MarketKind::Bars(Spacing::Minutes(120)),
            ..info
        };
        let text = serde_json::to_string(&bars).unwrap();
        assert!(text.contains(r#""kind":"bars","spacing":"2h""#), "{text}");
        assert_eq!(serde_json::from_str::<MarketInfo>(&text).unwrap(), bars);
        assert!(
            serde_json::from_str::<MarketInfo>(&text.replace(r#","spacing":"2h""#, "")).is_err()
        );
    }

    #[test]
    fn spacing_from_gaps_and_what_it_allows() {
        const MIN: UnixNanos = NANOS_PER_MINUTE;
        assert_eq!(Spacing::from_gap(60 * MIN), (Spacing::Minutes(60), false));
        assert_eq!(Spacing::from_gap(7 * MIN), (Spacing::Minutes(1), false));
        assert_eq!(
            Spacing::from_gap(10 * 1_000_000_000),
            (Spacing::Minutes(1), true)
        );
        assert_eq!(Spacing::from_gap(3 * DAY).0, Spacing::Minutes(1_440));
        assert_eq!(Spacing::from_gap(7 * DAY).0, Spacing::Week);
        assert_eq!(Spacing::from_gap(31 * DAY).0, Spacing::Month);
        // Daily bars with weekends still come out daily.
        let weekdays: Vec<UnixNanos> = (0..30).filter(|d| d % 7 < 5).map(|d| d * DAY).collect();
        assert_eq!(
            Spacing::from_times(&weekdays).unwrap().0,
            Spacing::Minutes(1_440)
        );
        assert_eq!(Spacing::from_times(&[5]), None);

        for text in ["3m", "2h", "1d", "1w", "1M"] {
            assert_eq!(text.parse::<Spacing>().unwrap().to_string(), text);
        }
        assert!("7m".parse::<Spacing>().is_err() && "2d".parse::<Spacing>().is_err());

        let two_hours = Spacing::Minutes(120);
        assert!(!two_hours.allows(Timeframe::H1));
        assert!(two_hours.allows(Timeframe::H4) && two_hours.allows(Timeframe::MN1));
        assert_eq!(two_hours.shortest(), Timeframe::H4);
        assert_eq!(Spacing::Minutes(3).shortest(), Timeframe::M15);
        assert!(!Spacing::Week.allows(Timeframe::MN1));
        assert_eq!(Spacing::Month.shortest(), Timeframe::MN1);
    }

    #[test]
    fn bar_markets_store_by_year_and_roll_up() {
        let store = temp_store("bars");
        let market = store
            .create(MarketInfo {
                kind: MarketKind::Bars(Spacing::Minutes(1_440)),
                ..info(TickKind::Trades)
            })
            .unwrap();
        // Daily bars from Monday 2024-01-01 for 400 days, stamped at 22:00 like some brokers.
        let first = persephone_core::time::days_from_civil(2024, 1, 1) as UnixNanos;
        let daily: Vec<Candle> = (0..400)
            .map(|i| bar((first + i) * DAY + 22 * 60 * MIN, 100.0 + i as f64))
            .collect();
        let write = |market: LocalMarket, bars: &[Candle]| {
            let mut writer = BarWriter::new(market).unwrap();
            for year in [2024, 2025] {
                let of_year: Vec<Candle> = bars
                    .iter()
                    .copied()
                    .filter(|b| year_of(b.ts) == year)
                    .collect();
                writer.write_year(year, of_year).unwrap();
            }
            writer.finish().unwrap()
        };
        let market = write(market, &daily);
        assert_eq!(market.info.count, 400);
        assert_eq!(market.info.first, first * DAY, "moved to the day's open");
        assert!(market.dir().join("bars/2025.parquet").exists());
        assert_eq!(
            write(market, &daily).info.count,
            400,
            "importing twice replaces"
        );

        let market = store.market("LOCAL", "EURUSD").unwrap();
        assert_eq!(market.candles(Timeframe::D1, 50).unwrap().len(), 50);
        let weekly = market.candles(Timeframe::W1, 1_000).unwrap();
        assert_eq!(weekly.len(), 58, "57 whole weeks and a part");
        assert_eq!((weekly[0].ts, weekly[0].volume), (first * DAY, 70.0));
        let monthly = market.candles(Timeframe::MN1, 3).unwrap();
        assert_eq!(monthly.len(), 3);
        assert_eq!(
            monthly[2].ts,
            persephone_core::time::days_from_civil(2025, 2, 1) as UnixNanos * DAY
        );
        assert!(
            market.candles(Timeframe::H1, 10).is_err(),
            "finer than its bars"
        );
        assert!(market.candles_by(BarSpec::Ticks(100), 10).is_err());
        assert!(market.read_ticks(0, u64::MAX).is_err());
        assert!(
            MarketWriter::new(market.clone())
                .write_day(first as i64, trades(first as i64, 0..1, 1.0))
                .is_err()
        );

        let day = |y, m, d| persephone_core::time::days_from_civil(y, m, d) as UnixNanos * DAY;
        let january = market
            .candles_between(Timeframe::D1.into(), day(2024, 1, 1), day(2024, 2, 1))
            .unwrap();
        assert_eq!(january.len(), 31);
        let around_new_year = market
            .candles_between(Timeframe::MN1.into(), day(2024, 12, 1), day(2025, 3, 1))
            .unwrap();
        assert_eq!(
            around_new_year.len(),
            3,
            "December to February, across year files"
        );
        assert!(
            market
                .candles_between(Timeframe::H1.into(), 0, u64::MAX)
                .is_err()
        );
        assert!(
            market
                .candles_between(BarSpec::Ticks(5), 0, u64::MAX)
                .is_err()
        );

        assert!(market.disk_size() > 0);
        store.remove("LOCAL", "EURUSD").unwrap();
        assert!(!market.dir().exists());
        assert!(store.markets().is_empty());
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn tick_markets_load_any_range() {
        let store = temp_store("between");
        let market = store.create(info(TickKind::Trades)).unwrap();
        let mut writer = MarketWriter::new(market);
        // Two weeks of a trade a minute from Monday 2024-01-29, across two month files.
        let monday = persephone_core::time::days_from_civil(2024, 1, 29);
        for day in monday..monday + 14 {
            writer.write_day(day, trades(day, 0..1_440, 1.0)).unwrap();
        }
        let market = writer.finish().unwrap();
        let at = |day: i64, hour: u64| day as UnixNanos * DAY + hour * 60 * MIN;
        let between = |spec: BarSpec, from, to| market.candles_between(spec, from, to).unwrap();

        let hours = between(Timeframe::H1.into(), at(monday + 2, 12), at(monday + 4, 12));
        assert_eq!(hours.len(), 48, "Wednesday noon to Friday noon");
        assert_eq!(hours[0].ts, at(monday + 2, 12));
        assert_eq!(hours[47].ts, at(monday + 4, 11));

        let weeks = between(Timeframe::W1.into(), at(monday, 0), at(monday + 14, 0));
        assert_eq!(weeks.len(), 2);
        assert_eq!(
            weeks[0].volume,
            7.0 * 1_440.0,
            "the whole week, from both months"
        );
        assert_eq!(
            between(Timeframe::W1.into(), at(monday + 1, 0), at(monday + 14, 0)).len(),
            1,
            "a week that opened before the range is left out"
        );
        assert_eq!(
            between(BarSpec::Ticks(60), at(monday, 0), at(monday, 2)).len(),
            2
        );
        assert!(between(Timeframe::H1.into(), at(monday, 1), at(monday, 1)).is_empty());
        assert_eq!(market.span(), (market.info.first, market.info.last));
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn tick_markets_make_weeks_across_months() {
        let store = temp_store("weeks");
        let market = store.create(info(TickKind::Trades)).unwrap();
        let mut writer = MarketWriter::new(market);
        // Monday 2024-01-29 to Sunday 2024-02-04 is one week across two month files.
        let monday = persephone_core::time::days_from_civil(2024, 1, 29);
        for day in monday..monday + 7 {
            writer.write_day(day, trades(day, 0..60, 1.0)).unwrap();
        }
        let market = writer.finish().unwrap();
        let weekly = market.candles(Timeframe::W1, 10).unwrap();
        assert_eq!(weekly.len(), 1);
        assert_eq!(
            (weekly[0].ts, weekly[0].volume),
            (monday as UnixNanos * DAY, 420.0)
        );
        assert_eq!(market.candles(Timeframe::MN1, 10).unwrap().len(), 2);
        fs::remove_dir_all(store.root()).unwrap();
    }
}

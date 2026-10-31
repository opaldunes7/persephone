//! Closed Binance bars kept on disk, so repeating a backtest costs nothing on the network and gives
//! the same answer.
//!
//! Each market remembers which stretches of time it has asked Binance for, not just the bars it got:
//! a range of bars can have holes that are really there (an exchange outage) and holes that are only
//! not fetched yet, and only the second kind should cost a request. Anything asked for that isn't
//! covered gets fetched, wherever it falls: before, after or between what's there.
//!
//! One market downloads at a time, in this process and between processes, so a batch of workers
//! starting cold makes one set of requests. Files are written aside and moved into place, so a reader
//! never sees half of one, and a cancelled or failed download only loses what's unfinished.

use std::{
    collections::HashMap,
    fs::{self, File},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use persephone_core::{Candle, InstrumentSpec, Timeframe, UnixNanos};
use persephone_data::binance::BinanceClient;
use serde_json::{Value, json};

/// Where bars come from: Binance, or a stand-in in tests.
pub trait BarSource: Send + Sync {
    /// # Errors
    ///
    /// Returns a message if the market can't be found.
    fn instrument(&self, symbol: &str) -> Result<InstrumentSpec, String>;

    /// The bars opening in `start..end`.
    ///
    /// # Errors
    ///
    /// Returns a message if they can't be fetched.
    fn klines(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        start: UnixNanos,
        end: UnixNanos,
    ) -> Result<Vec<Candle>, String>;

    /// The time now.
    fn now(&self) -> UnixNanos {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as UnixNanos)
    }
}

/// Binance itself.
pub struct Binance;

impl BarSource for Binance {
    fn instrument(&self, symbol: &str) -> Result<InstrumentSpec, String> {
        BinanceClient::new()
            .and_then(|client| client.instrument(symbol))
            .map_err(|e| format!("{e:#}"))
    }

    fn klines(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        start: UnixNanos,
        end: UnixNanos,
    ) -> Result<Vec<Candle>, String> {
        BinanceClient::new()
            .and_then(|client| client.klines(symbol, timeframe, start, end))
            .map_err(|e| format!("{e:#}"))
    }
}

/// How many bars to fetch before saving, so a long download's progress survives a failure.
const SAVE_EVERY_BARS: u64 = 50_000;
/// How many markets to hold in memory.
const IN_MEMORY: usize = 8;
/// How often to ask again about a market's details, which hardly ever change.
const INSTRUMENT_DAYS: u64 = 7;
const MAGIC: &[u8; 4] = b"PLB1";
/// A bar on disk: its time and five numbers.
const BAR_BYTES: usize = 48;

/// One market's bars and what's been asked for, as far as this process knows.
#[derive(Default)]
struct Market {
    bars: Arc<Vec<Candle>>,
    /// Stretches of bar opens already fetched, sorted and not touching.
    covered: Vec<(UnixNanos, UnixNanos)>,
    /// The file these came from, to notice another process has changed it.
    stamp: Option<SystemTime>,
    last_used: u64,
}

/// Which parts of `from..to` aren't in `covered`.
fn missing(
    covered: &[(UnixNanos, UnixNanos)],
    from: UnixNanos,
    to: UnixNanos,
) -> Vec<(UnixNanos, UnixNanos)> {
    let mut gaps = Vec::new();
    let mut at = from;
    for &(start, end) in covered {
        if end <= at {
            continue;
        }
        if start >= to {
            break;
        }
        if start > at {
            gaps.push((at, start));
        }
        at = at.max(end);
    }
    if at < to {
        gaps.push((at, to));
    }
    gaps
}

/// `covered` with `from..to` added, joining what now touches.
fn cover(covered: &mut Vec<(UnixNanos, UnixNanos)>, from: UnixNanos, to: UnixNanos) {
    covered.push((from, to));
    covered.sort_unstable();
    let mut joined: Vec<(UnixNanos, UnixNanos)> = Vec::with_capacity(covered.len());
    for &(start, end) in covered.iter() {
        match joined.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => joined.push((start, end)),
        }
    }
    *covered = joined;
}

/// Closed bars from a source, kept on disk.
pub struct BarCache {
    dir: PathBuf,
    source: Arc<dyn BarSource>,
    /// Each market's own lock is what makes one download at a time: workers wanting the same market
    /// wait here, and find it's been fetched.
    markets: Mutex<HashMap<String, Arc<Mutex<Market>>>>,
    uses: Mutex<u64>,
}

impl BarCache {
    #[must_use]
    pub fn new(dir: PathBuf, source: Arc<dyn BarSource>) -> Self {
        Self {
            dir,
            source,
            markets: Mutex::default(),
            uses: Mutex::new(0),
        }
    }

    /// The time now, by the source's clock.
    #[must_use]
    pub fn now(&self) -> UnixNanos {
        self.source.now()
    }

    /// When the bar still forming began: nothing from here on is final.
    #[must_use]
    pub fn forming(&self, timeframe: Timeframe) -> UnixNanos {
        timeframe.open_time(self.source.now())
    }

    fn path(&self, symbol: &str, timeframe: Timeframe) -> PathBuf {
        self.dir
            .join(format!("{symbol}-{}.bars", timeframe.label()))
    }

    /// A market's details, asked for once in a while.
    ///
    /// # Errors
    ///
    /// Returns a message if they aren't known and can't be fetched.
    pub fn instrument(&self, symbol: &str) -> Result<InstrumentSpec, String> {
        let path = self.dir.join(format!("{symbol}.instrument"));
        let fresh = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(INSTRUMENT_DAYS * 86_400));
        if fresh
            && let Some(known) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| instrument_from(&value))
        {
            return Ok(known);
        }
        match self.source.instrument(symbol) {
            Ok(found) => {
                let saved = json!({
                    "symbol": found.symbol,
                    "venue": found.venue,
                    "base": found.base,
                    "quote": found.quote,
                    "price_precision": found.price_precision,
                    "size_precision": found.size_precision,
                });
                let _ = fs::create_dir_all(&self.dir)
                    .and_then(|()| fs::write(&path, saved.to_string()));
                Ok(found)
            }
            // Out of date is better than nothing if Binance can't be reached.
            Err(error) => fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| instrument_from(&value))
                .ok_or(error),
        }
    }

    /// The closed bars opening in `from..to`, oldest first, fetching whatever isn't here yet.
    ///
    /// # Errors
    ///
    /// Returns a message if what's missing can't be fetched or saved.
    pub fn bars(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        from: UnixNanos,
        to: UnixNanos,
    ) -> Result<Vec<Candle>, String> {
        let forming = self.forming(timeframe);
        let start = timeframe.open_time(from);
        let open = timeframe.open_time(to.min(forming));
        // Up to a bar's end, as a bar that's opened by `to` is one asked for.
        let end = if open >= to.min(forming) {
            open
        } else {
            timeframe.close_time(to.min(forming)).min(forming)
        };
        if start >= end {
            return Ok(Vec::new());
        }
        let market = self.market(symbol, timeframe);
        let mut market = market
            .lock()
            .map_err(|_| "the bar cache failed".to_owned())?;
        let path = self.path(symbol, timeframe);
        self.refresh(&mut market, &path)?;
        if !missing(&market.covered, start, end).is_empty() {
            self.fetch(&mut market, &path, symbol, timeframe, (start, end))?;
        }
        let bars = &market.bars;
        let first = bars.partition_point(|bar| bar.ts < from.max(start));
        let last = bars.partition_point(|bar| bar.ts < end.min(to));
        Ok(bars[first..last.max(first)].to_vec())
    }

    /// The entry for a market, leaving out those least recently used if there are too many.
    fn market(&self, symbol: &str, timeframe: Timeframe) -> Arc<Mutex<Market>> {
        let key = format!("{symbol}-{}", timeframe.label());
        let mut uses = self.uses.lock().unwrap_or_else(|e| e.into_inner());
        *uses += 1;
        let mut markets = self.markets.lock().unwrap_or_else(|e| e.into_inner());
        if markets.len() >= IN_MEMORY && !markets.contains_key(&key) {
            // One nobody's using: a busy market's lock can't be taken, so it stays.
            let oldest = markets
                .iter()
                .filter_map(|(key, market)| {
                    let market = market.try_lock().ok()?;
                    Some((market.last_used, key.clone()))
                })
                .min();
            if let Some((_, oldest)) = oldest {
                markets.remove(&oldest);
            }
        }
        let market = markets.entry(key).or_default();
        if let Ok(mut held) = market.try_lock() {
            held.last_used = *uses;
        }
        Arc::clone(market)
    }

    /// Reads the file if it's new to us: first use, or another process has added to it.
    fn refresh(&self, market: &mut Market, path: &PathBuf) -> Result<(), String> {
        let stamp = fs::metadata(path).and_then(|meta| meta.modified()).ok();
        if stamp.is_some() && stamp != market.stamp {
            let (bars, covered) = read_file(path)?;
            market.bars = Arc::new(bars);
            market.covered = covered;
            market.stamp = stamp;
        }
        Ok(())
    }

    fn fetch(
        &self,
        market: &mut Market,
        path: &PathBuf,
        symbol: &str,
        timeframe: Timeframe,
        (start, end): (UnixNanos, UnixNanos),
    ) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        // Other processes wait here too; what they fetched is then read, not fetched again.
        let lock = File::create(path.with_extension("lock")).map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| e.to_string())?;
        self.refresh(market, path)?;
        let step = SAVE_EVERY_BARS.saturating_mul(timeframe.nanos());
        for (gap_start, gap_end) in missing(&market.covered, start, end) {
            let mut at = gap_start;
            while at < gap_end {
                let until = gap_end.min(at.saturating_add(step));
                let fetched = self.source.klines(symbol, timeframe, at, until)?;
                merge(market, fetched);
                // Even with no bars: that's a real hole in the market, not one to ask about again.
                cover(&mut market.covered, at, until);
                write_file(path, &market.bars, &market.covered)?;
                market.stamp = fs::metadata(path).and_then(|meta| meta.modified()).ok();
                at = until;
            }
        }
        Ok(())
    }
}

/// Adds bars, in order; a bar fetched again replaces the one there.
fn merge(market: &mut Market, fetched: Vec<Candle>) {
    let Some(last) = fetched.last() else {
        return;
    };
    let bars = Arc::make_mut(&mut market.bars);
    // Nearly always they simply go on the end.
    if bars.last().is_none_or(|bar| bar.ts < fetched[0].ts) {
        bars.extend(fetched);
        return;
    }
    let (first_ts, last_ts) = (fetched[0].ts, last.ts);
    let from = bars.partition_point(|bar| bar.ts < first_ts);
    let to = bars.partition_point(|bar| bar.ts <= last_ts);
    bars.splice(from..to, fetched);
}

fn instrument_from(value: &Value) -> Option<InstrumentSpec> {
    let text = |name: &str| value[name].as_str().map(str::to_owned);
    Some(InstrumentSpec {
        symbol: text("symbol")?,
        venue: text("venue")?,
        base: text("base")?,
        quote: text("quote")?,
        price_precision: u8::try_from(value["price_precision"].as_u64()?).ok()?,
        size_precision: u8::try_from(value["size_precision"].as_u64()?).ok()?,
    })
}

/// A span of bars the file covers, from and to.
type Range = (UnixNanos, UnixNanos);

fn write_file(
    path: &PathBuf,
    bars: &[Candle],
    covered: &[(UnixNanos, UnixNanos)],
) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(16 + covered.len() * 16 + bars.len() * BAR_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(covered.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(bars.len() as u64).to_le_bytes());
    for (from, to) in covered {
        bytes.extend_from_slice(&from.to_le_bytes());
        bytes.extend_from_slice(&to.to_le_bytes());
    }
    for bar in bars {
        bytes.extend_from_slice(&bar.ts.to_le_bytes());
        for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|e| e.to_string())
}

fn read_file(path: &PathBuf) -> Result<(Vec<Candle>, Vec<Range>), String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let corrupt = || {
        format!(
            "{} isn't a bar cache; delete it to start over",
            path.display()
        )
    };
    if bytes.len() < 16 || &bytes[..4] != MAGIC {
        return Err(corrupt());
    }
    let word = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
    };
    let ranges = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| corrupt())?) as usize;
    let count = word(8).ok_or_else(corrupt)? as usize;
    if bytes.len() != 16 + ranges * 16 + count * BAR_BYTES {
        return Err(corrupt());
    }
    let mut covered = Vec::with_capacity(ranges);
    for i in 0..ranges {
        covered.push((
            word(16 + i * 16).ok_or_else(corrupt)?,
            word(24 + i * 16).ok_or_else(corrupt)?,
        ));
    }
    let start = 16 + ranges * 16;
    let float = |at: usize| word(at).map(f64::from_bits);
    let mut bars = Vec::with_capacity(count);
    for i in 0..count {
        let at = start + i * BAR_BYTES;
        bars.push(Candle {
            ts: word(at).ok_or_else(corrupt)?,
            open: float(at + 8).ok_or_else(corrupt)?,
            high: float(at + 16).ok_or_else(corrupt)?,
            low: float(at + 24).ok_or_else(corrupt)?,
            close: float(at + 32).ok_or_else(corrupt)?,
            volume: float(at + 40).ok_or_else(corrupt)?,
        });
    }
    Ok((bars, covered))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    const HOUR: u64 = 3_600_000_000_000;

    /// A market of one bar an hour, with the bars of `hole` not existing, that counts requests.
    struct Fake {
        now: AtomicU64,
        requests: Mutex<Vec<(u64, u64)>>,
        hole: Option<(u64, u64)>,
        fail_after: AtomicUsize,
        instruments: AtomicUsize,
    }

    impl Fake {
        fn new(hours: u64) -> Arc<Self> {
            Arc::new(Self {
                now: AtomicU64::new(hours * HOUR + 30 * 60_000_000_000),
                requests: Mutex::default(),
                hole: None,
                fail_after: AtomicUsize::new(usize::MAX),
                instruments: AtomicUsize::new(0),
            })
        }

        fn asked(&self) -> Vec<(u64, u64)> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn bar(hour: u64) -> Candle {
        let close = 100.0 + hour as f64 * 0.125;
        Candle {
            ts: hour * HOUR,
            open: close - 1.0,
            high: close + 1.0,
            low: close - 2.0,
            close,
            volume: 1.5,
        }
    }

    impl BarSource for Fake {
        fn instrument(&self, symbol: &str) -> Result<InstrumentSpec, String> {
            self.instruments.fetch_add(1, Ordering::Relaxed);
            Ok(InstrumentSpec {
                symbol: symbol.into(),
                venue: "BINANCE".into(),
                base: "BTC".into(),
                quote: "USDT".into(),
                price_precision: 2,
                size_precision: 5,
            })
        }

        fn klines(
            &self,
            _symbol: &str,
            _timeframe: Timeframe,
            start: UnixNanos,
            end: UnixNanos,
        ) -> Result<Vec<Candle>, String> {
            let done = self.requests.lock().unwrap().len();
            if done >= self.fail_after.load(Ordering::Relaxed) {
                return Err("the network went away".into());
            }
            self.requests
                .lock()
                .unwrap()
                .push((start / HOUR, end / HOUR));
            // Long enough for other workers to pile up behind it.
            std::thread::sleep(Duration::from_millis(5));
            Ok((start / HOUR..end / HOUR)
                .filter(|hour| self.hole.is_none_or(|(a, b)| *hour < a || *hour >= b))
                .map(bar)
                .collect())
        }

        fn now(&self) -> UnixNanos {
            self.now.load(Ordering::Relaxed)
        }
    }

    fn dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "persephone-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn bars(cache: &BarCache, from: u64, to: u64) -> Vec<Candle> {
        cache
            .bars("BTCUSDT", Timeframe::H1, from * HOUR, to * HOUR)
            .unwrap()
    }

    fn hours(bars: &[Candle]) -> Vec<u64> {
        bars.iter().map(|bar| bar.ts / HOUR).collect()
    }

    fn range(from: u64, to: u64) -> Vec<u64> {
        (from..to).collect()
    }

    #[test]
    fn asking_again_costs_nothing_and_gives_the_same_bars() {
        let fake = Fake::new(1_000);
        let cache = BarCache::new(dir(), fake.clone());
        let first = bars(&cache, 100, 200);
        assert_eq!(hours(&first), range(100, 200));
        assert_eq!(first[5], bar(105), "exactly what was fetched");
        assert_eq!(fake.asked(), vec![(100, 200)]);
        assert_eq!(bars(&cache, 100, 200), first);
        assert_eq!(hours(&bars(&cache, 120, 130)), range(120, 130), "inside it");
        assert_eq!(fake.asked().len(), 1, "nothing more was asked for");
    }

    #[test]
    fn it_survives_a_restart_exactly() {
        let fake = Fake::new(1_000);
        let dir = dir();
        let first = bars(&BarCache::new(dir.clone(), fake.clone()), 100, 200);
        let again = bars(&BarCache::new(dir, fake.clone()), 100, 200);
        assert_eq!(first, again);
        assert_eq!(fake.asked().len(), 1, "read from disk");
    }

    #[test]
    fn only_whats_missing_is_fetched_wherever_it_falls() {
        let fake = Fake::new(1_000);
        let cache = BarCache::new(dir(), fake.clone());
        bars(&cache, 100, 200);
        bars(&cache, 300, 400);
        // A later span, an earlier one, and across the hole between: only the gaps.
        assert_eq!(hours(&bars(&cache, 150, 450)), range(150, 450));
        assert_eq!(fake.asked()[2..], [(200, 300), (400, 450)]);
        assert_eq!(hours(&bars(&cache, 50, 120)), range(50, 120));
        assert_eq!(fake.asked()[4..], [(50, 100)]);
        assert_eq!(fake.asked().len(), 5);
        assert_eq!(
            hours(&bars(&cache, 50, 450)),
            range(50, 450),
            "all joined up"
        );
        assert_eq!(fake.asked().len(), 5);
    }

    #[test]
    fn a_hole_in_the_market_itself_isnt_asked_about_twice() {
        let mut fake = Fake::new(1_000);
        Arc::get_mut(&mut fake).unwrap().hole = Some((150, 160));
        let cache = BarCache::new(dir(), fake.clone());
        let got = bars(&cache, 100, 200);
        assert_eq!(got.len(), 90, "the bars that exist");
        bars(&cache, 100, 200);
        assert_eq!(fake.asked().len(), 1);
    }

    #[test]
    fn a_failure_midway_keeps_whats_done_and_finishes_later() {
        let fake = Fake::new(200_000);
        let dir = dir();
        let cache = BarCache::new(dir.clone(), fake.clone());
        // Three saves' worth, failing on the second.
        fake.fail_after.store(1, Ordering::Relaxed);
        assert!(
            cache
                .bars("BTCUSDT", Timeframe::H1, 0, 120_000 * HOUR)
                .is_err()
        );
        assert_eq!(fake.asked(), vec![(0, 50_000)]);

        // Even after a restart, it carries on from there rather than leaving a hole.
        fake.fail_after.store(usize::MAX, Ordering::Relaxed);
        let cache = BarCache::new(dir, fake.clone());
        let all = cache
            .bars("BTCUSDT", Timeframe::H1, 0, 120_000 * HOUR)
            .unwrap();
        assert_eq!(all.len(), 120_000);
        assert_eq!(fake.asked()[1], (50_000, 100_000), "not from the start");
    }

    #[test]
    fn workers_starting_cold_make_one_download() {
        let fake = Fake::new(1_000);
        let cache = Arc::new(BarCache::new(dir(), fake.clone()));
        let answers: Vec<Vec<Candle>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..10)
                .map(|_| {
                    let cache = Arc::clone(&cache);
                    scope.spawn(move || bars(&cache, 100, 300))
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        assert_eq!(fake.asked().len(), 1, "{:?}", fake.asked());
        assert!(
            answers
                .iter()
                .all(|answer| hours(answer) == range(100, 300))
        );
    }

    #[test]
    fn two_caches_on_one_folder_share_what_they_fetch() {
        // As two processes would: the second notices what the first saved.
        let fake = Fake::new(1_000);
        let dir = dir();
        let (a, b) = (
            BarCache::new(dir.clone(), fake.clone()),
            BarCache::new(dir, fake.clone()),
        );
        bars(&b, 500, 510);
        bars(&a, 100, 200);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(hours(&bars(&b, 100, 200)), range(100, 200));
        assert_eq!(fake.asked().len(), 2, "b read what a fetched");
        assert_eq!(
            hours(&bars(&a, 500, 510)),
            range(500, 510),
            "and neither lost any"
        );
        assert_eq!(fake.asked().len(), 2);
    }

    #[test]
    fn the_bar_still_forming_is_never_given_or_remembered() {
        // It's half past hour 1,000, so that bar isn't finished.
        let fake = Fake::new(1_000);
        let cache = BarCache::new(dir(), fake.clone());
        let got = bars(&cache, 990, 2_000);
        assert_eq!(hours(&got), range(990, 1_000), "up to the last closed bar");

        // Once it's closed it's fetched for real, the moment it's asked for.
        fake.now.store(1_001 * HOUR + 1, Ordering::Relaxed);
        assert_eq!(hours(&bars(&cache, 990, 2_000)), range(990, 1_001));
        assert_eq!(*fake.asked().last().unwrap(), (1_000, 1_001));
    }

    #[test]
    fn a_span_ending_inside_a_bar_includes_that_bar() {
        let fake = Fake::new(1_000);
        let cache = BarCache::new(dir(), fake.clone());
        let half = HOUR / 2;
        let got = cache
            .bars("BTCUSDT", Timeframe::H1, 100 * HOUR, 110 * HOUR + half)
            .unwrap();
        assert_eq!(hours(&got), range(100, 111));
    }

    #[test]
    fn a_damaged_file_says_so() {
        let fake = Fake::new(1_000);
        let dir = dir();
        let cache = BarCache::new(dir.clone(), fake.clone());
        bars(&cache, 100, 110);
        let file = dir.join("BTCUSDT-1h.bars");
        let bytes = fs::read(&file).unwrap();
        fs::write(&file, &bytes[..bytes.len() - 3]).unwrap();
        let error = BarCache::new(dir, fake)
            .bars("BTCUSDT", Timeframe::H1, 100 * HOUR, 110 * HOUR)
            .unwrap_err();
        assert!(error.contains("delete it"), "{error}");
    }

    #[test]
    fn a_markets_details_are_only_asked_for_once_in_a_while() {
        let fake = Fake::new(10);
        let dir = dir();
        let cache = BarCache::new(dir.clone(), fake.clone());
        let found = cache.instrument("BTCUSDT").unwrap();
        assert_eq!(cache.instrument("BTCUSDT").unwrap(), found);
        assert_eq!(
            BarCache::new(dir, fake.clone())
                .instrument("BTCUSDT")
                .unwrap(),
            found
        );
        assert_eq!(fake.instruments.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn gaps_are_worked_out_right() {
        let covered = [(10, 20), (30, 40)];
        assert_eq!(missing(&covered, 0, 50), vec![(0, 10), (20, 30), (40, 50)]);
        assert_eq!(missing(&covered, 12, 18), vec![]);
        assert_eq!(missing(&covered, 15, 35), vec![(20, 30)]);
        assert_eq!(missing(&[], 1, 2), vec![(1, 2)]);
        let mut joined = vec![(10, 20), (30, 40)];
        cover(&mut joined, 20, 30);
        assert_eq!(joined, vec![(10, 40)]);
        cover(&mut joined, 50, 60);
        assert_eq!(joined, vec![(10, 40), (50, 60)]);
        cover(&mut joined, 5, 70);
        assert_eq!(joined, vec![(5, 70)]);
    }
}

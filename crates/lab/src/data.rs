//! Tools for markets, bars and ticks.

use std::{
    fs::{self, File},
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use persephone_core::{BarSpec, Candle, Timeframe, UnixNanos, tick::Ticks};
use persephone_data::{
    archive,
    binance::BinanceClient,
    import::{self, MarketOptions, Overrides},
};
use persephone_engine::FeeRates;
use persephone_research::market::{MarketBars, load_binance, load_local};
use serde_json::{Value, json};

use crate::{
    Lab,
    jobs::Job,
    new_id, parse_time, time_text,
    tools::{limit_of, text},
};

const BAR_COLUMNS: [&str; 7] = ["ts", "time", "open", "high", "low", "close", "volume"];
const DEFAULT_PAGE: usize = 1_000;

/// A market as a call names it.
#[derive(Clone, Debug)]
pub(crate) struct MarketArgs {
    pub symbol: String,
    pub venue: Option<String>,
    pub bars: BarSpec,
    pub from: Option<UnixNanos>,
    pub to: Option<UnixNanos>,
}

impl MarketArgs {
    pub(crate) fn parse(args: &Value) -> Result<Self, String> {
        let bars = args.get("bars").and_then(Value::as_str).unwrap_or("1h");
        let time = |name: &str| {
            args.get(name)
                .map(|value| parse_time(value).map_err(|e| format!("`{name}`: {e}")))
                .transpose()
        };
        Ok(Self {
            symbol: text(args, "symbol")?.to_uppercase(),
            venue: args
                .get("venue")
                .and_then(Value::as_str)
                .map(str::to_uppercase),
            bars: bars
                .parse()
                .map_err(|_| format!("`bars`: `{bars}` isn't 1m, 1h, 1d, 100t, 5v or the like"))?,
            from: time("from")?,
            to: time("to")?,
        })
    }

    fn timeframe(&self) -> Result<Timeframe, String> {
        self.bars.timeframe().ok_or_else(|| {
            "Binance serves time bars; tick and volume bars need a local market".to_owned()
        })
    }

    /// The span to load: both ends, or none for the latest bars.
    pub(crate) fn span(&self) -> Option<(UnixNanos, UnixNanos)> {
        self.from
            .map(|from| (from, self.to.unwrap_or(UnixNanos::MAX)))
    }
}

fn now_nanos() -> UnixNanos {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as UnixNanos)
}

fn error_text(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn bar_row(candle: &Candle) -> Value {
    json!([
        candle.ts,
        time_text(candle.ts),
        candle.open,
        candle.high,
        candle.low,
        candle.close,
        candle.volume
    ])
}

impl Lab {
    /// A market's bars with what the engine needs, with bars before `from` to warm up on.
    pub(crate) fn load_market(&self, market: &MarketArgs) -> Result<MarketBars, String> {
        let span = market.span();
        match &market.venue {
            Some(venue) => load_local(&self.store, venue, &market.symbol, market.bars, span),
            None => {
                let span = span.map(|(from, to)| (from, to.min(now_nanos())));
                load_binance(&market.symbol, market.timeframe()?, span)
            }
        }
        .map_err(error_text)
    }

    pub(crate) fn list_markets(&self) -> Result<Value, String> {
        let local: Vec<Value> = self
            .store
            .markets()
            .iter()
            .map(|market| {
                let info = &market.info;
                json!({
                    "symbol": info.symbol,
                    "venue": info.venue,
                    "base": info.base,
                    "quote": info.quote,
                    "holds": info.kind.to_string(),
                    "first": time_text(info.first),
                    "last": time_text(info.last),
                    "count": info.count,
                    "fee": info.fee,
                    "bytes": market.disk_size(),
                })
            })
            .collect();
        Ok(json!({
            "local": local,
            "binance": "Any Binance spot symbol, like BTCUSDT, without a venue.",
        }))
    }

    pub(crate) fn get_instrument(&self, args: &Value) -> Result<Value, String> {
        let symbol = text(args, "symbol")?.to_uppercase();
        let (instrument, fees) = match args.get("venue").and_then(Value::as_str) {
            Some(venue) => {
                let market = self
                    .store
                    .market(&venue.to_uppercase(), &symbol)
                    .map_err(error_text)?;
                let fee = market.info.fee;
                (
                    market.instrument(),
                    FeeRates {
                        maker: fee,
                        taker: fee,
                    },
                )
            }
            None => (
                BinanceClient::new()
                    .and_then(|client| client.instrument(&symbol))
                    .map_err(error_text)?,
                FeeRates::BINANCE_SPOT,
            ),
        };
        Ok(json!({
            "symbol": instrument.symbol,
            "venue": instrument.venue,
            "base": instrument.base,
            "quote": instrument.quote,
            "price_precision": instrument.price_precision,
            "size_precision": instrument.size_precision,
            "fees": {"maker": fees.maker, "taker": fees.taker},
        }))
    }

    pub(crate) fn get_history_range(&self, args: &Value) -> Result<Value, String> {
        let market = MarketArgs::parse(args)?;
        let (first, last) = match &market.venue {
            Some(venue) => self
                .store
                .market(venue, &market.symbol)
                .map_err(error_text)?
                .span(),
            None => {
                let timeframe = market.timeframe()?;
                (
                    BinanceClient::new()
                        .and_then(|client| client.first_bar_time(&market.symbol, timeframe))
                        .map_err(error_text)?,
                    now_nanos(),
                )
            }
        };
        Ok(
            json!({"first": time_text(first), "last": time_text(last), "first_ts": first, "last_ts": last}),
        )
    }

    /// Exactly the bars asked for, with no warm-up: at most `limit` from `from`, when given.
    fn bars_between(&self, market: &MarketArgs, limit: usize) -> Result<Vec<Candle>, String> {
        let Some(from) = market.from else {
            // The latest ones.
            let mut bars = self.load_market(market)?.candles;
            let start = bars.len().saturating_sub(limit);
            return Ok(bars.split_off(start));
        };
        let to = market.to.unwrap_or(UnixNanos::MAX);
        match &market.venue {
            Some(venue) => self
                .store
                .market(venue, &market.symbol)
                .and_then(|m| m.candles_between(market.bars, from, to))
                .map_err(error_text),
            None => {
                let timeframe = market.timeframe()?;
                let end = to
                    .min(now_nanos().saturating_add(timeframe.nanos()))
                    .min(from.saturating_add(limit as u64 * timeframe.nanos()));
                BinanceClient::new()
                    .and_then(|client| client.klines(&market.symbol, timeframe, from, end))
                    .map_err(error_text)
            }
        }
    }

    pub(crate) fn get_bars(&self, args: &Value) -> Result<Value, String> {
        let mut market = MarketArgs::parse(args)?;
        // A cursor is simply where the next bar starts.
        if let Some(cursor) = args.get("cursor").and_then(Value::as_str) {
            market.from = Some(
                cursor
                    .parse()
                    .map_err(|_| format!("`{cursor}` isn't a cursor"))?,
            );
        }
        let limit = limit_of(args, DEFAULT_PAGE);
        let mut bars = self.bars_between(&market, limit)?;
        let more = bars.len() > limit;
        bars.truncate(limit);
        // Binance's bars stop at the page's end, so there may be more when a page is full.
        let full = market.venue.is_none() && market.from.is_some() && bars.len() == limit;
        let next = (more || full)
            .then(|| bars.last().map(|bar| (bar.ts + 1).to_string()))
            .flatten();
        Ok(json!({
            "columns": BAR_COLUMNS,
            "rows": bars.iter().map(bar_row).collect::<Vec<_>>(),
            "next_cursor": next,
        }))
    }

    pub(crate) fn get_ticks(&self, args: &Value) -> Result<Value, String> {
        let symbol = text(args, "symbol")?.to_uppercase();
        let venue = text(args, "venue")?.to_uppercase();
        let market = self.store.market(&venue, &symbol).map_err(error_text)?;
        let mut from = parse_time(args.get("from").unwrap_or(&Value::Null))
            .map_err(|e| format!("`from`: {e}"))?;
        if let Some(cursor) = args.get("cursor").and_then(Value::as_str) {
            from = cursor
                .parse()
                .map_err(|_| format!("`{cursor}` isn't a cursor"))?;
        }
        let to = args
            .get("to")
            .map(|value| parse_time(value).map_err(|e| format!("`to`: {e}")))
            .transpose()?
            .unwrap_or(UnixNanos::MAX);
        let limit = limit_of(args, DEFAULT_PAGE);
        let ticks = market.read_ticks(from, to).map_err(error_text)?;
        let (columns, mut rows, times) = tick_rows(&ticks);
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = more.then(|| (times[limit - 1] + 1).to_string());
        Ok(json!({
            "kind": match ticks { Ticks::Trades(_) => "trades", Ticks::Quotes(_) => "quotes" },
            "columns": columns,
            "rows": rows,
            "next_cursor": next,
        }))
    }

    pub(crate) fn export_data(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let market = MarketArgs::parse(args)?;
        let table = if args.get("ticks").and_then(Value::as_bool).unwrap_or(false) {
            let venue = market
                .venue
                .as_ref()
                .ok_or("ticks need a local market's `venue`")?;
            let local = self
                .store
                .market(venue, &market.symbol)
                .map_err(error_text)?;
            let from = market.from.ok_or("`from` is required for ticks")?;
            let to = market.to.unwrap_or(UnixNanos::MAX);
            Table::from_ticks(&local.read_ticks(from, to).map_err(error_text)?)
        } else {
            Table::from_bars(&self.all_bars(&market, job)?)
        };
        if job.stop.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let format = args.get("format").and_then(Value::as_str).unwrap_or("csv");
        let path = match args.get("path").and_then(Value::as_str) {
            Some(path) => PathBuf::from(path),
            None => self.paths.lab().join("exports").join(format!(
                "{}-{}.{format}",
                market.symbol,
                new_id("export")
            )),
        };
        if path.exists() {
            return Err(format!(
                "{} already exists; Lab won't overwrite it",
                path.display()
            ));
        }
        match format {
            "csv" => table.write_csv(&path),
            "parquet" => table.write_parquet(&path),
            other => return Err(format!("`format`: `{other}` isn't csv or parquet")),
        }
        .map_err(error_text)?;
        Ok(
            json!({"path": path, "rows": table.times.len(), "format": format, "columns": table.names}),
        )
    }

    /// All the bars asked for however many there are, Binance's a page at a time.
    fn all_bars(&self, market: &MarketArgs, job: &Arc<Job>) -> Result<Vec<Candle>, String> {
        const PAGE: usize = 50_000;
        if market.venue.is_some() || market.from.is_none() {
            return self.bars_between(market, usize::MAX >> 16);
        }
        let mut all = Vec::new();
        let mut page = market.clone();
        while !job.stop.load(Ordering::Relaxed) {
            let bars = self.bars_between(&page, PAGE)?;
            let Some(last) = bars.last() else { break };
            page.from = Some(last.ts + 1);
            let done = bars.len() < PAGE;
            all.extend(bars);
            job.done.store(all.len(), Ordering::Relaxed);
            if done {
                break;
            }
        }
        Ok(all)
    }

    pub(crate) fn import_data(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let files: Vec<PathBuf> = args
            .get("files")
            .and_then(Value::as_array)
            .ok_or("`files` is missing")?
            .iter()
            .map(|file| {
                file.as_str()
                    .map(PathBuf::from)
                    .ok_or_else(|| format!("`files`: expected paths, not {file}"))
            })
            .collect::<Result<_, _>>()?;
        let optional = |name: &str| {
            args.get(name)
                .and_then(Value::as_str)
                .map(str::to_uppercase)
        };
        let options = MarketOptions {
            symbol: text(args, "symbol")?.to_uppercase(),
            venue: text(args, "venue")?.to_uppercase(),
            base: optional("base"),
            quote: optional("quote"),
            fee: args.get("fee").and_then(Value::as_f64),
        };
        let summary = self.import_files(&files, &options, job)?;
        Ok(summary)
    }

    fn import_files(
        &self,
        files: &[PathBuf],
        options: &MarketOptions,
        job: &Arc<Job>,
    ) -> Result<Value, String> {
        let first = files.first().ok_or("`files` is empty")?;
        let sample = import::sample(first).map_err(error_text)?;
        let detection = import::detect(&sample, &Overrides::default())
            .map_err(|e| format!("working out the layout of {}: {e:#}", first.display()))?;
        let stop = Arc::clone(&job.stop);
        let summary = import::import(&self.store, files, &detection.format, options, &mut |p| {
            job.done.store(p.rows as usize, Ordering::Relaxed);
            !stop.load(Ordering::Relaxed)
        })
        .map_err(error_text)?;
        if summary.stopped {
            return Err("Cancelled; what was read so far stays in the store.".into());
        }
        Ok(json!({
            "market": summary.market.id(),
            "imported": summary.imported,
            "skipped": summary.skipped,
            "problems": summary.problems,
            "first": time_text(summary.first),
            "last": time_text(summary.last),
        }))
    }

    pub(crate) fn fetch_archive(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let symbol = text(args, "symbol")?.to_uppercase();
        let from = text(args, "from")?;
        let to = args.get("to").and_then(Value::as_str).unwrap_or(from);
        let kind = if args
            .get("all_trades")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            archive::Kind::Trades
        } else {
            archive::Kind::AggTrades
        };
        let periods = archive::periods(from, to).map_err(error_text)?;
        let downloads = self.paths.lab().join("downloads");
        fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;
        // Monthly archives run to gigabytes: no overall time limit.
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("persephone-lab/", env!("CARGO_PKG_VERSION")))
            .timeout(None::<Duration>)
            .build()
            .map_err(|e| e.to_string())?;
        job.total.store(periods.len(), Ordering::Relaxed);
        let mut files = Vec::new();
        for (done, period) in periods.into_iter().enumerate() {
            if job.stop.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let dest = downloads.join(archive::file_name(&symbol, kind, period));
            let url = archive::url(archive::ARCHIVE_URL, &symbol, kind, period);
            if dest.exists()
                || archive::download(&http, &url, &dest, &mut |_, _| {}).map_err(error_text)?
            {
                files.push(dest);
            }
            job.done.store(done + 1, Ordering::Relaxed);
        }
        if files.is_empty() {
            return Err("None of those archives exist; check the symbol and dates.".into());
        }
        job.done.store(0, Ordering::Relaxed);
        let options = MarketOptions {
            symbol,
            venue: "BINANCE".into(),
            base: None,
            quote: None,
            fee: None,
        };
        let mut summary = self.import_files(&files, &options, job)?;
        let names: Vec<String> = files
            .iter()
            .map(|file| file.display().to_string())
            .collect();
        if let Some(object) = summary.as_object_mut() {
            object.insert("files".into(), json!(names));
        }
        Ok(summary)
    }
}

/// Ticks as rows, with their times for finding where a page ends.
fn tick_rows(ticks: &Ticks) -> (Vec<&'static str>, Vec<Value>, Vec<UnixNanos>) {
    match ticks {
        Ticks::Trades(trades) => (
            vec!["ts", "price", "size", "aggressor"],
            trades
                .iter()
                .map(|t| json!([t.ts, t.price, t.size, format!("{:?}", t.aggressor)]))
                .collect(),
            trades.iter().map(|t| t.ts).collect(),
        ),
        Ticks::Quotes(quotes) => (
            vec!["ts", "bid", "ask", "bid_size", "ask_size"],
            quotes
                .iter()
                .map(|q| json!([q.ts, q.bid, q.ask, q.bid_size, q.ask_size]))
                .collect(),
            quotes.iter().map(|q| q.ts).collect(),
        ),
    }
}

/// Columns of numbers by time, for writing to a file.
struct Table {
    names: Vec<&'static str>,
    times: Vec<UnixNanos>,
    columns: Vec<Vec<f64>>,
}

impl Table {
    fn from_bars(bars: &[Candle]) -> Self {
        let column = |get: fn(&Candle) -> f64| bars.iter().map(get).collect();
        Self {
            names: vec!["open", "high", "low", "close", "volume"],
            times: bars.iter().map(|bar| bar.ts).collect(),
            columns: vec![
                column(|b| b.open),
                column(|b| b.high),
                column(|b| b.low),
                column(|b| b.close),
                column(|b| b.volume),
            ],
        }
    }

    fn from_ticks(ticks: &Ticks) -> Self {
        match ticks {
            Ticks::Trades(trades) => Self {
                names: vec!["price", "size"],
                times: trades.iter().map(|t| t.ts).collect(),
                columns: vec![
                    trades.iter().map(|t| t.price).collect(),
                    trades.iter().map(|t| t.size).collect(),
                ],
            },
            Ticks::Quotes(quotes) => Self {
                names: vec!["bid", "ask", "bid_size", "ask_size"],
                times: quotes.iter().map(|q| q.ts).collect(),
                columns: vec![
                    quotes.iter().map(|q| q.bid).collect(),
                    quotes.iter().map(|q| q.ask).collect(),
                    quotes.iter().map(|q| q.bid_size).collect(),
                    quotes.iter().map(|q| q.ask_size).collect(),
                ],
            },
        }
    }

    fn write_csv(&self, path: &Path) -> anyhow::Result<()> {
        let mut out = BufWriter::new(File::create_new(path)?);
        writeln!(out, "ts,{}", self.names.join(","))?;
        for (row, ts) in self.times.iter().enumerate() {
            write!(out, "{ts}")?;
            for column in &self.columns {
                write!(out, ",{}", column[row])?;
            }
            writeln!(out)?;
        }
        out.flush()?;
        Ok(())
    }

    fn write_parquet(&self, path: &Path) -> anyhow::Result<()> {
        let mut fields = vec![Field::new("ts", DataType::Int64, false)];
        fields.extend(
            self.names
                .iter()
                .map(|name| Field::new(*name, DataType::Float64, false)),
        );
        let schema = Arc::new(Schema::new(fields));
        let times: Vec<i64> = self.times.iter().map(|&ts| ts as i64).collect();
        let mut arrays: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(times))];
        arrays.extend(
            self.columns
                .iter()
                .map(|column| Arc::new(Float64Array::from(column.clone())) as ArrayRef),
        );
        let batch = RecordBatch::try_new(Arc::clone(&schema), arrays)?;
        let mut writer = ArrowWriter::try_new(File::create_new(path)?, schema, None)?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    }
}

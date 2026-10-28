//! Binance spot market data over the public REST API (no API key needed).
//!
//! Uses `data-api.binance.vision`, Binance's market-data-only host, which is reachable from
//! more regions than `api.binance.com`.

use anyhow::{Context, bail};
use persephone_core::{Candle, InstrumentSpec, Timeframe, UnixNanos};
use serde::Deserialize;

const BASE_URL: &str = "https://data-api.binance.vision";
const VENUE: &str = "BINANCE";
/// Binance's maximum klines per request.
const PAGE_LIMIT: usize = 1000;
/// The most bars one range may hold: about 300 requests, and as much as a chart handles
/// comfortably.
pub const MAX_RANGE_BARS: u64 = 300_000;
const NANOS_PER_MILLI: u64 = 1_000_000;

pub struct BinanceClient {
    http: reqwest::blocking::Client,
}

impl BinanceClient {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn new() -> anyhow::Result<Self> {
        // A request that stalls fails instead of leaving a chart loading for good.
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("persephone/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { http })
    }

    /// Looks up a spot symbol's assets and tick/lot precision.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the symbol is unknown.
    pub fn instrument(&self, symbol: &str) -> anyhow::Result<InstrumentSpec> {
        let url = format!("{BASE_URL}/api/v3/exchangeInfo?symbol={symbol}");
        let info: ExchangeInfo = self
            .http
            .get(url)
            .send()?
            .error_for_status()
            .with_context(|| format!("Binance has no spot symbol `{symbol}`"))?
            .json()?;
        let raw = info
            .symbols
            .into_iter()
            .next()
            .with_context(|| format!("Binance has no spot symbol `{symbol}`"))?;
        raw.into_spec()
    }

    /// When the symbol's first bar of `timeframe` opened.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or Binance has no bars for it.
    pub fn first_bar_time(&self, symbol: &str, timeframe: Timeframe) -> anyhow::Result<UnixNanos> {
        let url = format!(
            "{BASE_URL}/api/v3/klines?symbol={symbol}&interval={}&startTime=0&limit=1",
            timeframe.label()
        );
        let body = self.http.get(url).send()?.error_for_status()?.text()?;
        parse_klines(&body)?
            .first()
            .map(|bar| bar.ts)
            .with_context(|| format!("Binance has no {timeframe} bars for {symbol}"))
    }

    /// Fetches every bar whose open time is in `[start, end)`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error if a request fails or a response cannot be parsed.
    pub fn klines(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        start: UnixNanos,
        end: UnixNanos,
    ) -> anyhow::Result<Vec<Candle>> {
        let mut candles = Vec::new();
        let mut cursor_ms = start / NANOS_PER_MILLI;
        let end_ms = end / NANOS_PER_MILLI;

        while cursor_ms < end_ms {
            let url = format!(
                "{BASE_URL}/api/v3/klines?symbol={symbol}&interval={}&startTime={cursor_ms}&endTime={}&limit={PAGE_LIMIT}",
                timeframe.label(),
                end_ms - 1,
            );
            let body = self.http.get(url).send()?.error_for_status()?.text()?;
            let page = parse_klines(&body)?;
            let Some(last) = page.last() else { break };

            cursor_ms = timeframe.close_time(last.ts) / NANOS_PER_MILLI;
            let full_page = page.len() == PAGE_LIMIT;
            candles.extend(page);
            if !full_page {
                break;
            }
        }

        Ok(candles)
    }
}

/// Checks that `[start, end)` holds at most [`MAX_RANGE_BARS`] bars of `timeframe`.
///
/// # Errors
///
/// Returns a message saying how many bars it holds and what to do instead.
pub fn check_range(timeframe: Timeframe, start: UnixNanos, end: UnixNanos) -> anyhow::Result<()> {
    let bars = end.saturating_sub(start) / timeframe.nanos();
    if bars > MAX_RANGE_BARS {
        bail!(
            "that range is {bars} {timeframe} bars, more than {MAX_RANGE_BARS}; pick a shorter range or a longer timeframe"
        );
    }
    Ok(())
}

/// Parses a `/api/v3/klines` response body.
///
/// Each row is `[open_time_ms, "open", "high", "low", "close", "volume", close_time_ms, ...]`.
///
/// # Errors
///
/// Returns an error if the body is not a kline array.
pub fn parse_klines(body: &str) -> anyhow::Result<Vec<Candle>> {
    let rows: Vec<Vec<serde_json::Value>> =
        serde_json::from_str(body).context("unexpected kline response")?;

    rows.iter()
        .map(|row| {
            if row.len() < 6 {
                bail!("kline row has {} fields, expected at least 6", row.len());
            }
            let open_ms = row[0]
                .as_u64()
                .context("kline open time is not an integer")?;
            let num = |i: usize| -> anyhow::Result<f64> {
                row[i]
                    .as_str()
                    .context("kline value is not a string")?
                    .parse()
                    .context("kline value is not a number")
            };
            Ok(Candle {
                ts: open_ms * NANOS_PER_MILLI,
                open: num(1)?,
                high: num(2)?,
                low: num(3)?,
                close: num(4)?,
                volume: num(5)?,
            })
        })
        .collect()
}

/// Decimal places in a Binance increment string: `"0.01000000"` → 2, `"1.00000000"` → 0.
fn precision_of(increment: &str) -> u8 {
    increment
        .split_once('.')
        .map_or(0, |(_, frac)| frac.trim_end_matches('0').len() as u8)
}

#[derive(Deserialize)]
struct ExchangeInfo {
    symbols: Vec<RawSymbol>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSymbol {
    symbol: String,
    base_asset: String,
    quote_asset: String,
    filters: Vec<serde_json::Value>,
}

impl RawSymbol {
    fn into_spec(self) -> anyhow::Result<InstrumentSpec> {
        let filter_value = |filter_type: &str, key: &str| -> anyhow::Result<String> {
            self.filters
                .iter()
                .find(|f| f["filterType"] == filter_type)
                .and_then(|f| f[key].as_str())
                .map(str::to_owned)
                .with_context(|| format!("{} has no {filter_type}.{key}", self.symbol))
        };
        let tick_size = filter_value("PRICE_FILTER", "tickSize")?;
        let step_size = filter_value("LOT_SIZE", "stepSize")?;

        Ok(InstrumentSpec {
            price_precision: precision_of(&tick_size),
            size_precision: precision_of(&step_size),
            symbol: self.symbol,
            venue: VENUE.to_owned(),
            base: self.base_asset,
            quote: self.quote_asset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kline_rows() {
        let body = r#"[[1792947600000,"80806.01000000","80951.57000000","80744.00000000","80796.00000000","320.81872000",1792951199999,"25928459.65",61758,"150.39","12154837.97","0"]]"#;
        let candles = parse_klines(body).unwrap();
        assert_eq!(
            candles,
            vec![Candle {
                ts: 1_792_947_600_000_000_000,
                open: 80806.01,
                high: 80951.57,
                low: 80744.0,
                close: 80796.0,
                volume: 320.81872,
            }]
        );
    }

    #[test]
    fn ranges_are_capped() {
        let year = 365 * 86_400_000_000_000;
        assert!(
            check_range(Timeframe::H1, 0, 30 * year).is_ok(),
            "about 263k hours"
        );
        let error = check_range(Timeframe::M1, 0, year).unwrap_err().to_string();
        assert!(error.contains("525600 1m bars"), "{error}");
        assert!(check_range(Timeframe::M1, year, 0).is_ok());
    }

    #[test]
    fn rejects_error_bodies() {
        assert!(parse_klines(r#"{"code":-1121,"msg":"Invalid symbol."}"#).is_err());
    }

    #[test]
    fn precision_from_increment() {
        assert_eq!(precision_of("0.01000000"), 2);
        assert_eq!(precision_of("0.00001000"), 5);
        assert_eq!(precision_of("1.00000000"), 0);
        assert_eq!(precision_of("10"), 0);
    }

    #[test]
    fn spec_from_exchange_info() {
        let raw: RawSymbol = serde_json::from_str(
            r#"{"symbol":"BTCUSDT","baseAsset":"BTC","quoteAsset":"USDT","filters":[
                {"filterType":"PRICE_FILTER","tickSize":"0.01000000"},
                {"filterType":"LOT_SIZE","stepSize":"0.00001000"}]}"#,
        )
        .unwrap();
        let spec = raw.into_spec().unwrap();
        assert_eq!(spec.symbol, "BTCUSDT");
        assert_eq!(spec.venue, "BINANCE");
        assert_eq!((spec.price_precision, spec.size_precision), (2, 5));
    }
}

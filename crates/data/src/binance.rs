//! Binance spot market data over the public REST API (no API key needed).
//!
//! Uses `data-api.binance.vision`, Binance's market-data-only host, which is reachable from
//! more regions than `api.binance.com`.

use std::time::Duration;

use anyhow::{Context, bail};
use persephone_core::{Candle, InstrumentSpec, Timeframe, UnixNanos};
use serde::Deserialize;

const BASE_URL: &str = "https://data-api.binance.vision";
/// Binance's USD-M futures, for perpetuals.
const FUTURES_URL: &str = "https://fapi.binance.com";
/// What marks a symbol as a perpetual here, as in `BTCUSDT-PERP`. Binance itself calls it `BTCUSDT`.
pub const PERP_SUFFIX: &str = "-PERP";

/// Whether `symbol` names a perpetual future.
#[must_use]
pub fn is_perp(symbol: &str) -> bool {
    symbol.ends_with(PERP_SUFFIX)
}
const VENUE: &str = "BINANCE";
/// Binance's maximum klines per request.
pub const PAGE_LIMIT: usize = 1000;
/// The most bars one range may hold: about 300 requests, and as much as a chart handles
/// comfortably.
pub const MAX_RANGE_BARS: u64 = 300_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
/// How many times to ask again after a request fails in a way that may pass.
const MOST_RETRIES: u32 = 5;
/// The longest to wait when told to slow down; a longer wait is a failure to report.
const MOST_RETRY_WAIT_SECS: u64 = 60;

pub struct BinanceClient {
    http: reqwest::blocking::Client,
    base: String,
    futures_base: String,
}

/// How long to wait before asking again after a response of `status`, if it's worth asking again:
/// 429 and 418 are Binance saying to slow down, with `Retry-After` saying for how long, and a 5xx
/// is its own trouble, which usually passes.
fn retry_wait(status: u16, retry_after: Option<u64>, retries: u32) -> Option<Duration> {
    if !matches!(status, 429 | 418 | 500..=599) || retries >= MOST_RETRIES {
        return None;
    }
    let seconds = retry_after.unwrap_or(1 << retries);
    (seconds <= MOST_RETRY_WAIT_SECS).then(|| Duration::from_secs(seconds))
}

impl BinanceClient {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn new() -> anyhow::Result<Self> {
        // A request that stalls fails instead of leaving a chart loading for good.
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("persephone/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base: BASE_URL.to_owned(),
            futures_base: FUTURES_URL.to_owned(),
        })
    }

    /// A successful response to `url`, waiting and asking again if told to slow down.
    fn get(&self, url: String) -> anyhow::Result<reqwest::blocking::Response> {
        let mut retries = 0;
        loop {
            let response = match self.http.get(&url).send() {
                Ok(response) => response,
                // One stalled or dropped connection shouldn't lose a long download.
                Err(error)
                    if (error.is_timeout() || error.is_connect()) && retries < MOST_RETRIES =>
                {
                    std::thread::sleep(Duration::from_secs(1 << retries));
                    retries += 1;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            match retry_wait(response.status().as_u16(), retry_after, retries) {
                Some(wait) => std::thread::sleep(wait),
                None => return Ok(response.error_for_status()?),
            }
            retries += 1;
        }
    }

    /// Looks up a spot symbol's assets and tick/lot precision.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the symbol is unknown.
    pub fn instrument(&self, symbol: &str) -> anyhow::Result<InstrumentSpec> {
        if let Some(future) = symbol.strip_suffix(PERP_SUFFIX) {
            // It lists them all, with no asking for just one.
            let url = format!("{}/fapi/v1/exchangeInfo", self.futures_base);
            let info: ExchangeInfo = self.get(url)?.json()?;
            let raw = info
                .symbols
                .into_iter()
                .find(|raw| {
                    raw.symbol == future && raw.contract_type.as_deref() == Some("PERPETUAL")
                })
                .with_context(|| format!("Binance has no perpetual `{future}`"))?;
            let mut spec = raw.into_spec()?;
            spec.symbol = symbol.to_owned();
            return Ok(spec);
        }
        let url = format!("{}/api/v3/exchangeInfo?symbol={symbol}", self.base);
        let info: ExchangeInfo = self
            .get(url)
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
        let (base, path, symbol) = self.klines_at(symbol);
        let url = format!(
            "{base}{path}?symbol={symbol}&interval={}&startTime=0&limit=1",
            timeframe.label()
        );
        let body = self.get(url)?.text()?;
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
        let (base, path, symbol) = self.klines_at(symbol);

        while cursor_ms < end_ms {
            let url = format!(
                "{base}{path}?symbol={symbol}&interval={}&startTime={cursor_ms}&endTime={}&limit={PAGE_LIMIT}",
                timeframe.label(),
                end_ms - 1,
            );
            let body = self.get(url)?.text()?;
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

impl BinanceClient {
    /// Where a symbol's bars are: its host and path, and what Binance calls it.
    fn klines_at<'a>(&self, symbol: &'a str) -> (&str, &'static str, &'a str) {
        match symbol.strip_suffix(PERP_SUFFIX) {
            Some(future) => (&self.futures_base, "/fapi/v1/klines", future),
            None => (&self.base, "/api/v3/klines", symbol),
        }
    }

    /// A perpetual's funding rates paid in `[start, end)`, as when and how much, oldest first.
    /// Anything else has none.
    ///
    /// # Errors
    ///
    /// Returns an error if a request fails or Binance's answer can't be read.
    pub fn funding(
        &self,
        symbol: &str,
        start: UnixNanos,
        end: UnixNanos,
    ) -> anyhow::Result<Vec<(UnixNanos, f64)>> {
        let Some(future) = symbol.strip_suffix(PERP_SUFFIX) else {
            return Ok(Vec::new());
        };
        const FUNDING_PAGE: usize = 1_000;
        let mut rates = Vec::new();
        let mut cursor_ms = start / NANOS_PER_MILLI;
        let end_ms = end / NANOS_PER_MILLI;
        while cursor_ms < end_ms {
            let url = format!(
                "{}/fapi/v1/fundingRate?symbol={future}&startTime={cursor_ms}&endTime={}&limit={FUNDING_PAGE}",
                self.futures_base,
                end_ms - 1,
            );
            let page = parse_funding(&self.get(url)?.text()?)?;
            let Some(&(last, _)) = page.last() else { break };
            cursor_ms = last / NANOS_PER_MILLI + 1;
            let full_page = page.len() == FUNDING_PAGE;
            rates.extend(page);
            if !full_page {
                break;
            }
        }
        Ok(rates)
    }
}

/// Parses a `/fapi/v1/fundingRate` response body.
///
/// # Errors
///
/// Returns an error if it isn't one, as when Binance answers with an error instead.
pub fn parse_funding(body: &str) -> anyhow::Result<Vec<(UnixNanos, f64)>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Rate {
        funding_time: u64,
        funding_rate: String,
    }
    let rates: Vec<Rate> =
        serde_json::from_str(body).with_context(|| format!("unexpected funding rates: {body}"))?;
    rates
        .into_iter()
        .map(|rate| {
            Ok((
                rate.funding_time * NANOS_PER_MILLI,
                rate.funding_rate
                    .parse()
                    .with_context(|| format!("a funding rate of `{}`", rate.funding_rate))?,
            ))
        })
        .collect()
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
    /// Futures only: `PERPETUAL`, or a delivery date's kind.
    #[serde(default)]
    contract_type: Option<String>,
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
mod funding_tests {
    use super::*;

    #[test]
    fn funding_rates_read_as_when_and_how_much() {
        let body = r#"[{"symbol":"BTCUSDT","fundingTime":1000,"fundingRate":"0.00010000","markPrice":"1"},
                       {"symbol":"BTCUSDT","fundingTime":2000,"fundingRate":"-0.00020000"}]"#;
        assert_eq!(
            parse_funding(body).unwrap(),
            [(1_000_000_000, 0.0001), (2_000_000_000, -0.0002)]
        );
        assert!(parse_funding(r#"{"code":-1121,"msg":"Invalid symbol."}"#).is_err());
        assert!(is_perp("BTCUSDT-PERP"));
        assert!(!is_perp("BTCUSDT"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn being_told_to_slow_down_waits_and_asks_again() {
        use std::{
            io::{Read as _, Write as _},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let answers = [
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n[]",
            ];
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.read(&mut [0; 4096]).unwrap();
                stream.write_all(answer.as_bytes()).unwrap();
            }
        });
        let mut client = super::BinanceClient::new().unwrap();
        client.base = format!("http://127.0.0.1:{port}");
        let bars = client
            .klines(
                "BTCUSDT",
                persephone_core::Timeframe::H1,
                0,
                3_600_000_000_000,
            )
            .unwrap();
        assert!(bars.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn only_slow_down_answers_are_tried_again_and_not_for_ever() {
        use std::time::Duration;

        use super::retry_wait;
        assert_eq!(retry_wait(429, Some(7), 0), Some(Duration::from_secs(7)));
        assert_eq!(retry_wait(418, None, 2), Some(Duration::from_secs(4)));
        assert_eq!(retry_wait(429, Some(3_600), 0), None, "too long to wait");
        assert_eq!(retry_wait(429, Some(1), 5), None, "tried enough");
        assert_eq!(retry_wait(503, Some(1), 0), Some(Duration::from_secs(1)));
        assert_eq!(retry_wait(404, Some(1), 0), None);
        assert_eq!(retry_wait(200, None, 0), None);
    }

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

//! Binance's bulk market data archives (data.binance.vision): one zipped CSV per symbol and
//! month (or day), each with a published SHA-256. Downloaded files import like any CSV.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, bail, ensure};
use persephone_core::time::{civil_from_days, days_from_civil};
use sha2::{Digest, Sha256};

pub const ARCHIVE_URL: &str = "https://data.binance.vision";

/// Which trades an archive holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Trades merged by price and taker, much smaller; what charts and most backtests need.
    AggTrades,
    /// Every single trade.
    Trades,
}

impl Kind {
    const fn path(self) -> &'static str {
        match self {
            Self::AggTrades => "aggTrades",
            Self::Trades => "trades",
        }
    }
}

/// A month or a day of archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Month { year: i64, month: u32 },
    Day { year: i64, month: u32, day: u32 },
}

impl Period {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Month { year, month } => format!("{year:04}-{month:02}"),
            Self::Day { year, month, day } => format!("{year:04}-{month:02}-{day:02}"),
        }
    }

    const fn folder(self) -> &'static str {
        match self {
            Self::Month { .. } => "monthly",
            Self::Day { .. } => "daily",
        }
    }
}

/// Every month from `from` to `to` (`2024-01`), or every day (`2024-01-15`), inclusive.
///
/// # Errors
///
/// Returns a message if the dates aren't both months or both days, or run backwards.
pub fn periods(from: &str, to: &str) -> anyhow::Result<Vec<Period>> {
    let parse = |text: &str| -> anyhow::Result<Vec<u32>> {
        text.split('-')
            .map(|part| {
                part.parse::<u32>()
                    .with_context(|| format!("`{text}` isn't a date"))
            })
            .collect()
    };
    let (a, b) = (parse(from)?, parse(to)?);
    let periods = match (a.as_slice(), b.as_slice()) {
        ([y1, m1], [y2, m2]) => {
            let index = |y: u32, m: u32| i64::from(y) * 12 + i64::from(m) - 1;
            ensure!(
                (1..=12).contains(m1) && (1..=12).contains(m2),
                "months run 1 to 12"
            );
            (index(*y1, *m1)..=index(*y2, *m2))
                .map(|i| Period::Month {
                    year: i.div_euclid(12),
                    month: (i.rem_euclid(12) + 1) as u32,
                })
                .collect::<Vec<_>>()
        }
        ([y1, m1, d1], [y2, m2, d2]) => {
            let start = days_from_civil(i64::from(*y1), *m1, *d1);
            let end = days_from_civil(i64::from(*y2), *m2, *d2);
            (start..=end)
                .map(|days| {
                    let (year, month, day) = civil_from_days(days);
                    Period::Day { year, month, day }
                })
                .collect()
        }
        _ => bail!("give both dates as months (2024-01) or both as days (2024-01-15)"),
    };
    ensure!(!periods.is_empty(), "`{from}` comes after `{to}`");
    Ok(periods)
}

/// The archive file's name, e.g. `BTCUSDT-aggTrades-2024-01.zip`.
#[must_use]
pub fn file_name(symbol: &str, kind: Kind, period: Period) -> String {
    format!("{symbol}-{}-{}.zip", kind.path(), period.label())
}

/// Where the archive file is.
#[must_use]
pub fn url(base: &str, symbol: &str, kind: Kind, period: Period) -> String {
    format!(
        "{}/data/spot/{}/{}/{symbol}/{}",
        base.trim_end_matches('/'),
        period.folder(),
        kind.path(),
        file_name(symbol, kind, period)
    )
}

/// Downloads `url` to `dest`, checking it against its published SHA-256 as it streams in.
/// Returns `false` when there's no such archive (the symbol didn't trade then, or that month
/// isn't published yet). `progress` gets bytes so far and the total, if known.
///
/// # Errors
///
/// Returns an error if the download fails or doesn't match its checksum (the partial file is
/// removed).
pub fn download(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> anyhow::Result<bool> {
    let checksum = http
        .get(format!("{url}.CHECKSUM"))
        .send()
        .with_context(|| format!("fetching {url}.CHECKSUM"))?;
    if checksum.status().as_u16() == 404 {
        return Ok(false);
    }
    let checksum = checksum.error_for_status()?.text()?;
    let expected = checksum
        .split_whitespace()
        .next()
        .filter(|hex| hex.len() == 64)
        .with_context(|| format!("{url}.CHECKSUM doesn't hold a SHA-256"))?
        .to_lowercase();

    let mut response = http.get(url).send()?.error_for_status()?;
    let total = response.content_length();
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let partial = dest.with_extension("zip.part");
    let mut file =
        fs::File::create(&partial).with_context(|| format!("writing {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let mut done = 0;
    loop {
        let n = response
            .read(&mut buffer)
            .with_context(|| format!("downloading {url}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
        done += n as u64;
        progress(done, total);
    }
    file.flush()?;
    drop(file);
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        let _ = fs::remove_file(&partial);
        bail!("{url} doesn't match its checksum; try again");
    }
    fs::rename(&partial, dest)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[test]
    fn periods_cover_months_and_days() {
        let months: Vec<String> = periods("2023-11", "2024-02")
            .unwrap()
            .iter()
            .map(|p| p.label())
            .collect();
        assert_eq!(months, ["2023-11", "2023-12", "2024-01", "2024-02"]);
        let days = periods("2024-02-28", "2024-03-01").unwrap();
        assert_eq!(days.len(), 3, "2024 is a leap year");
        assert!(periods("2024-03", "2024-01").is_err());
        assert!(periods("2024-03", "2024-03-01").is_err());
        assert_eq!(
            url(ARCHIVE_URL, "BTCUSDT", Kind::AggTrades, days[1]),
            "https://data.binance.vision/data/spot/daily/aggTrades/BTCUSDT/BTCUSDT-aggTrades-2024-02-29.zip"
        );
    }

    /// Serves `files` (path → body) and answers 404 otherwise.
    fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                }
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                match files.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    }
                    None => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                    }
                }
            }
        });
        base
    }

    #[test]
    fn downloads_are_checked_against_their_checksum() {
        let body = b"pretend zip".to_vec();
        let sum: String = Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let good = Period::Month {
            year: 2024,
            month: 1,
        };
        let bad = Period::Month {
            year: 2024,
            month: 2,
        };
        let path = |p| url("", "BTCUSDT", Kind::AggTrades, p);
        let base = serve(vec![
            (path(good), body.clone()),
            (
                format!("{}.CHECKSUM", path(good)),
                format!("{sum}  BTCUSDT-aggTrades-2024-01.zip").into_bytes(),
            ),
            (path(bad), b"tampered".to_vec()),
            (
                format!("{}.CHECKSUM", path(bad)),
                format!("{sum}  BTCUSDT-aggTrades-2024-02.zip").into_bytes(),
            ),
        ]);
        let dir = std::env::temp_dir().join(format!("persephone-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let http = reqwest::blocking::Client::new();
        let mut seen = 0;

        let dest = dir.join(file_name("BTCUSDT", Kind::AggTrades, good));
        assert!(
            download(
                &http,
                &url(&base, "BTCUSDT", Kind::AggTrades, good),
                &dest,
                &mut |n, _| seen = n
            )
            .unwrap()
        );
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert_eq!(seen, body.len() as u64);

        let dest = dir.join(file_name("BTCUSDT", Kind::AggTrades, bad));
        let error = download(
            &http,
            &url(&base, "BTCUSDT", Kind::AggTrades, bad),
            &dest,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");
        assert!(!dest.exists() && !dest.with_extension("zip.part").exists());

        let missing = Period::Month {
            year: 2019,
            month: 1,
        };
        let dest = dir.join(file_name("BTCUSDT", Kind::AggTrades, missing));
        assert!(
            !download(
                &http,
                &url(&base, "BTCUSDT", Kind::AggTrades, missing),
                &dest,
                &mut |_, _| {}
            )
            .unwrap()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

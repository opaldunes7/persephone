//! Headlines from public RSS and Atom feeds, kept on disk as they arrive so a history builds up,
//! and older ones from GDELT's archive of the world's news, which goes back to 2017.
//!
//! Feeds only carry their last few dozen items, so on their own there's nothing from before
//! collecting began. An item's time is when its publisher put it out, or for GDELT when GDELT
//! first saw it, which can be after the news first broke, never before. Good for seeing what was
//! being said around a bar, not for trading on to the second.

use std::{
    collections::HashSet,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use persephone_core::{
    UnixNanos,
    time::{civil_from_days, days_from_civil},
};
use serde::{Deserialize, Serialize};

/// Public feeds that need no key: crypto news, and the Fed's and the SEC's own releases.
pub const DEFAULT_FEEDS: [&str; 5] = [
    "https://www.coindesk.com/arc/outboundfeeds/rss/",
    "https://cointelegraph.com/rss",
    "https://decrypt.co/feed",
    "https://www.federalreserve.gov/feeds/press_all.xml",
    "https://www.sec.gov/news/pressreleases.rss",
];

/// How much a headline may move markets, by a rough rule on its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Importance {
    Low,
    /// It names the market being looked at.
    Medium,
    /// Rates, inflation, regulators, hacks, failures and the like.
    High,
}

/// One headline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    /// When its feed says it was published, or when it was first seen if the feed doesn't say.
    pub published: UnixNanos,
    /// When it was first fetched here.
    pub seen: UnixNanos,
    /// The feed's own name.
    pub source: String,
    pub title: String,
    pub link: String,
    /// The feed's id for it, or its link.
    pub id: String,
    /// From a regulator or central bank's own feed.
    #[serde(default)]
    pub official: bool,
}

/// Words that mark a headline as one markets move on.
const HIGH_WORDS: [&str; 24] = [
    "fed ",
    "fomc",
    "federal reserve",
    "rate decision",
    "rate cut",
    "rate hike",
    "interest rate",
    "cpi",
    "inflation",
    "jobs report",
    "payrolls",
    "sec ",
    "etf approv",
    "etf reject",
    "hack",
    "exploit",
    "halt",
    "suspend",
    "bankrupt",
    "insolven",
    "liquidat",
    "ban ",
    "lawsuit",
    "charges",
];

impl Headline {
    /// How much it may matter to a market named by `words` (its symbol's base asset and the
    /// names it goes by): High for market-moving words or a regulator's own release, Medium if it
    /// names the market, else Low.
    #[must_use]
    pub fn importance(&self, words: &[&str]) -> Importance {
        let title = format!(" {} ", self.title.to_lowercase());
        if self.official || HIGH_WORDS.iter().any(|word| title.contains(word)) {
            Importance::High
        } else if words.iter().any(|word| names(&title, word)) {
            Importance::Medium
        } else {
            Importance::Low
        }
    }

    /// Whether it's about the economy or regulators rather than one market.
    #[must_use]
    pub fn is_macro(&self) -> bool {
        self.official || self.importance(&[]) == Importance::High
    }

    /// Whether it names a market called by `words`.
    #[must_use]
    pub fn names_market(&self, words: &[&str]) -> bool {
        let title = format!(" {} ", self.title.to_lowercase());
        words.iter().any(|word| names(&title, word))
    }
}

/// Whether `word` is in `text` (lowercase, padded with spaces) as a word of its own.
fn names(text: &str, word: &str) -> bool {
    let word = word.to_lowercase();
    text.match_indices(&word).any(|(at, _)| {
        let before = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after = text[at + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        before && after
    })
}

/// What a market is called in headlines, from its symbol: `BTCUSDT` is BTC and Bitcoin.
#[must_use]
pub fn market_words(symbol: &str) -> Vec<String> {
    let symbol = symbol.to_uppercase();
    let symbol = symbol.trim_end_matches("-PERP");
    let base = ["USDT", "USDC", "FDUSD", "BUSD", "USD", "EUR", "BTC", "ETH"]
        .iter()
        .find_map(|quote| symbol.strip_suffix(quote).filter(|base| !base.is_empty()))
        .unwrap_or(symbol);
    let mut words = vec![base.to_owned()];
    let name = match base {
        "BTC" => Some("Bitcoin"),
        "ETH" => Some("Ethereum"),
        "SOL" => Some("Solana"),
        "XRP" => Some("Ripple"),
        "BNB" => Some("Binance Coin"),
        "DOGE" => Some("Dogecoin"),
        "ADA" => Some("Cardano"),
        "AVAX" => Some("Avalanche"),
        "DOT" => Some("Polkadot"),
        "LINK" => Some("Chainlink"),
        "LTC" => Some("Litecoin"),
        "TRX" => Some("Tron"),
        "TON" => Some("Toncoin"),
        _ => None,
    };
    words.extend(name.map(ToOwned::to_owned));
    if base == "ETH" {
        words.push("Ether".into());
    }
    words
}

fn now() -> UnixNanos {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as UnixNanos)
}

/// A feed's headlines, read from its text: RSS 0.9 to 2.0, Atom or JSON Feed. An item with no
/// date is taken as published when seen, `seen`.
///
/// # Errors
///
/// Returns why it isn't a feed.
pub fn parse_feed(text: &[u8], url: &str, seen: UnixNanos) -> Result<Vec<Headline>, String> {
    let feed = feed_rs::parser::parse(text).map_err(|e| format!("{url}: not a feed: {e}"))?;
    let source = feed
        .title
        .map(|title| title.content.trim().to_owned())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| url.to_owned());
    let official = [
        "federalreserve.gov",
        "sec.gov",
        "ecb.europa.eu",
        "bankofengland",
    ]
    .iter()
    .any(|host| url.contains(host));
    Ok(feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let title = entry
                .title?
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if title.is_empty() {
                return None;
            }
            let link = entry
                .links
                .first()
                .map(|link| link.href.clone())
                .unwrap_or_default();
            let published = entry
                .published
                .or(entry.updated)
                .and_then(|at| at.timestamp_nanos_opt())
                .and_then(|nanos| UnixNanos::try_from(nanos).ok())
                // A date in the future is the feed's mistake; it's no later than when seen.
                .map_or(seen, |at| at.min(seen));
            let id = if entry.id.is_empty() {
                link.clone()
            } else {
                entry.id
            };
            Some(Headline {
                published,
                seen,
                source: source.clone(),
                title,
                link,
                id,
                official,
            })
        })
        .collect())
}

/// GDELT's article search: the world's news back to 2017, free and needing no key.
pub const GDELT: &str = "https://api.gdeltproject.org/api/v2/doc/doc";
/// GDELT asks for no more than one request every 5 seconds.
pub const GDELT_PAUSE: Duration = Duration::from_secs(5);
/// When its archive begins: 1 January 2017.
pub const GDELT_FROM: UnixNanos = 1_483_228_800_000_000_000;
/// How long a stretch one request covers. It gives the most relevant 250 in it.
pub const HISTORY_DAYS: u64 = 14;
const HISTORY_RECORDS: &str = "250";
const NANOS_PER_DAY: u64 = 86_400_000_000_000;
/// What's searched for the economy and regulators.
pub const MACRO_QUERY: &str = "(\"federal reserve\" OR fomc OR \"interest rates\" OR inflation OR \"rate cut\" OR \"rate hike\" OR \"crypto regulation\")";

/// What's searched for a market called by `words`: the names it goes by, in English. GDELT
/// refuses words under three letters, so those are left out.
#[must_use]
pub fn market_query(words: &[&str]) -> Option<String> {
    let terms: Vec<String> = words
        .iter()
        .filter(|word| word.chars().count() >= 3)
        .map(|word| {
            if word.contains(' ') {
                format!("\"{}\"", word.to_lowercase())
            } else {
                word.to_lowercase()
            }
        })
        .collect();
    match terms.len() {
        0 => None,
        1 => Some(terms[0].clone()),
        _ => Some(format!("({})", terms.join(" OR "))),
    }
}

/// The stretches GDELT is asked about to cover `[from, to)`, each `HISTORY_DAYS` long and
/// starting on a fixed grid from `GDELT_FROM`, so one fetched once is known by its start.
#[must_use]
pub fn history_stretches(from: UnixNanos, to: UnixNanos) -> Vec<(UnixNanos, UnixNanos)> {
    let step = HISTORY_DAYS * NANOS_PER_DAY;
    let from = from.max(GDELT_FROM);
    if to <= from {
        return Vec::new();
    }
    let mut at = GDELT_FROM + (from - GDELT_FROM) / step * step;
    let mut stretches = Vec::new();
    while at < to {
        stretches.push((at, at + step));
        at += step;
    }
    stretches
}

/// `YYYYMMDDHHMMSS`, as GDELT takes times.
fn gdelt_time(at: UnixNanos) -> String {
    let seconds = at / 1_000_000_000;
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let of_day = seconds % 86_400;
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// `20240105T123000Z`, as GDELT gives when it saw an article.
fn parse_gdelt_time(text: &str) -> Option<UnixNanos> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 14 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| digits[range].parse::<u64>().ok();
    let days = days_from_civil(
        number(0..4)? as i64,
        number(4..6)? as u32,
        number(6..8)? as u32,
    );
    let seconds = u64::try_from(days).ok()? * 86_400
        + number(8..10)? * 3_600
        + number(10..12)? * 60
        + number(12..14)?;
    Some(seconds * 1_000_000_000)
}

#[derive(Deserialize)]
struct GdeltAnswer {
    #[serde(default)]
    articles: Vec<GdeltArticle>,
}

#[derive(Deserialize)]
struct GdeltArticle {
    url: String,
    #[serde(default)]
    title: String,
    seendate: String,
    #[serde(default)]
    domain: String,
}

/// GDELT's answer to an article search, as headlines. It answers `{}` when there are none, and a
/// line of text when it refuses the search.
///
/// # Errors
///
/// Returns what GDELT said when it isn't an answer.
pub fn parse_gdelt(text: &[u8], seen: UnixNanos) -> Result<Vec<Headline>, String> {
    let answer: GdeltAnswer = serde_json::from_slice(text).map_err(|_| {
        let said = String::from_utf8_lossy(text);
        format!(
            "GDELT: {}",
            said.trim().chars().take(200).collect::<String>()
        )
    })?;
    Ok(answer
        .articles
        .into_iter()
        .filter_map(|article| {
            let title = article
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let published = parse_gdelt_time(&article.seendate)?.min(seen);
            (!title.is_empty()).then(|| Headline {
                published,
                seen,
                official: ["federalreserve.gov", "sec.gov"]
                    .iter()
                    .any(|host| article.domain.ends_with(host)),
                source: article.domain,
                title,
                id: article.url.clone(),
                link: article.url,
            })
        })
        .collect())
}

/// Reads feeds over the network.
pub struct FeedClient {
    http: reqwest::blocking::Client,
}

impl FeedClient {
    /// # Errors
    ///
    /// Returns an error if the HTTP client can't be built.
    pub fn new() -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("persephone/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { http })
    }

    /// One feed's headlines now.
    ///
    /// # Errors
    ///
    /// Returns what went wrong, naming the feed.
    pub fn fetch(&self, url: &str) -> Result<Vec<Headline>, String> {
        let body = self
            .http
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::bytes)
            .map_err(|e| format!("{url}: {e}"))?;
        parse_feed(&body, url, now())
    }

    /// What GDELT has for `query` published in `[from, to)`, the most relevant first. Ask no more
    /// than once every `GDELT_PAUSE`.
    ///
    /// # Errors
    ///
    /// Returns what went wrong.
    pub fn history(
        &self,
        query: &str,
        from: UnixNanos,
        to: UnixNanos,
    ) -> Result<Vec<Headline>, String> {
        let query = format!("{query} sourcelang:english");
        let body = self
            .http
            .get(GDELT)
            .query(&[
                ("query", query.as_str()),
                ("mode", "artlist"),
                ("format", "json"),
                ("sort", "hybridrel"),
                ("maxrecords", HISTORY_RECORDS),
                ("startdatetime", gdelt_time(from).as_str()),
                ("enddatetime", gdelt_time(to).as_str()),
            ])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::bytes)
            .map_err(|e| format!("GDELT: {e}"))?;
        parse_gdelt(&body, now())
    }

    /// Every feed's headlines, and what went wrong with any that couldn't be read.
    #[must_use]
    pub fn fetch_all(&self, urls: &[String]) -> (Vec<Headline>, Vec<String>) {
        let (mut found, mut failed) = (Vec::new(), Vec::new());
        for url in urls {
            match self.fetch(url) {
                Ok(headlines) => found.extend(headlines),
                Err(error) => failed.push(error),
            }
        }
        (found, failed)
    }
}

/// The feeds to read: those listed one a line in `news/feeds.txt` under `root`, or the defaults.
/// Lines starting with `#` are notes.
#[must_use]
pub fn feeds(root: &Path) -> Vec<String> {
    let listed: Vec<String> = fs::read_to_string(root.join("news").join("feeds.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    if listed.is_empty() {
        DEFAULT_FEEDS.iter().map(|url| (*url).to_owned()).collect()
    } else {
        listed
    }
}

/// One search of GDELT's history still to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySearch {
    pub query: String,
    pub from: UnixNanos,
    pub to: UnixNanos,
    /// What it's known by once made.
    pub key: String,
}

/// Every headline collected so far, oldest first, kept in `news/headlines.jsonl` under the data
/// folder, one a line, only ever added to.
#[derive(Debug, Default)]
pub struct NewsStore {
    path: Option<PathBuf>,
    headlines: Vec<Headline>,
    known: HashSet<String>,
    /// The history searches already made, by what was searched and the stretch's start.
    searched: HashSet<String>,
}

impl NewsStore {
    /// The store under `root`, read in; a line that can't be read is skipped.
    #[must_use]
    pub fn open(root: &Path) -> Self {
        let path = root.join("news").join("headlines.jsonl");
        let mut store = Self {
            path: Some(path.clone()),
            ..Self::default()
        };
        let read: Vec<Headline> = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        store.merge_in(read);
        store.searched = fs::read_to_string(Self::searched_path(&path))
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        store
    }

    fn searched_path(headlines: &Path) -> PathBuf {
        headlines.with_file_name("searched.txt")
    }

    /// The key a history search is known by.
    #[must_use]
    pub fn search_key(query: &str, from: UnixNanos) -> String {
        format!("{from} {query}")
    }

    /// Whether this search of the history has been made already.
    #[must_use]
    pub fn searched(&self, key: &str) -> bool {
        self.searched.contains(key)
    }

    /// Notes a search of the history as made, so it isn't made again.
    ///
    /// # Errors
    ///
    /// Returns a message if it can't be written; it's kept in memory regardless.
    pub fn mark_searched(&mut self, key: String) -> Result<(), String> {
        if !self.searched.insert(key.clone()) {
            return Ok(());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::searched_path(path))
            .map_err(|e| e.to_string())?;
        file.write_all(format!("{key}\n").as_bytes())
            .map_err(|e| e.to_string())
    }

    /// A store kept only in memory, for tests.
    #[must_use]
    pub fn in_memory(headlines: Vec<Headline>) -> Self {
        let mut store = Self::default();
        store.merge_in(headlines);
        store
    }

    fn key(headline: &Headline) -> String {
        if headline.id.is_empty() {
            headline.link.clone()
        } else {
            headline.id.clone()
        }
    }

    /// Adds those it hasn't got, and returns how many that was.
    fn merge_in(&mut self, headlines: Vec<Headline>) -> Vec<Headline> {
        let mut added = Vec::new();
        for headline in headlines {
            let key = Self::key(&headline);
            let link = headline.link.clone();
            if self.known.contains(&key) || (!link.is_empty() && self.known.contains(&link)) {
                continue;
            }
            self.known.insert(key);
            if !link.is_empty() {
                self.known.insert(link);
            }
            added.push(headline);
        }
        self.headlines.extend(added.iter().cloned());
        self.headlines
            .sort_by(|a, b| a.published.cmp(&b.published).then(a.title.cmp(&b.title)));
        added
    }

    /// Adds new headlines, writing them to disk, and returns those that were new.
    ///
    /// # Errors
    ///
    /// Returns a message if they can't be written; they're kept in memory regardless.
    pub fn add(&mut self, headlines: Vec<Headline>) -> Result<Vec<Headline>, String> {
        let added = self.merge_in(headlines);
        if let (Some(path), false) = (&self.path, added.is_empty()) {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            let mut text = String::new();
            for headline in &added {
                text.push_str(&serde_json::to_string(headline).map_err(|e| e.to_string())?);
                text.push('\n');
            }
            // One write, so a reader never sees half a line.
            file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(added)
    }

    /// Every headline, oldest first.
    #[must_use]
    pub fn all(&self) -> &[Headline] {
        &self.headlines
    }

    /// Those published by `at`, newest first, at most `limit`: never one from after it.
    #[must_use]
    pub fn until(&self, at: UnixNanos, limit: usize) -> Vec<&Headline> {
        let end = self.headlines.partition_point(|h| h.published <= at);
        self.headlines[..end].iter().rev().take(limit).collect()
    }

    /// Those published in `[from, to)`, oldest first.
    #[must_use]
    pub fn between(&self, from: UnixNanos, to: UnixNanos) -> &[Headline] {
        let start = self.headlines.partition_point(|h| h.published < from);
        let end = self.headlines.partition_point(|h| h.published < to);
        &self.headlines[start..end.max(start)]
    }

    /// The history searches still to make for a market searched for by `query` over `[from, to)`:
    /// each stretch's search for the market and for the economy, unless made already. Stretches
    /// that end after `now` aren't counted as made, as more may still come in.
    #[must_use]
    pub fn searches_left(
        &self,
        query: Option<&str>,
        from: UnixNanos,
        to: UnixNanos,
    ) -> Vec<HistorySearch> {
        let mut left = Vec::new();
        for (start, end) in history_stretches(from, to) {
            for query in query.into_iter().chain(std::iter::once(MACRO_QUERY)) {
                let key = Self::search_key(query, start);
                if !self.searched(&key) {
                    left.push(HistorySearch {
                        query: query.to_owned(),
                        from: start,
                        to: end,
                        key,
                    });
                }
            }
        }
        left
    }

    /// When the first headline here was published, if there is one: nothing's known before it.
    #[must_use]
    pub fn since(&self) -> Option<UnixNanos> {
        self.headlines.first().map(|h| h.published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEEN: UnixNanos = 1_800_000_000_000_000_000;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Crypto Wire</title>
<item><title>Bitcoin   climbs as
 traders eye the Fed</title><link>https://example.com/a</link><guid>a-1</guid>
<pubDate>Sun, 10 Mar 2024 14:30:05 GMT</pubDate></item>
<item><title>Solana upgrade ships</title><link>https://example.com/b</link>
<pubDate>Sat, 09 Mar 2024 08:00:00 +0000</pubDate></item>
<item><title>No date given</title><link>https://example.com/c</link></item>
<item><title></title><link>https://example.com/empty</link></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Press Releases</title>
<entry><title>FOMC statement</title><id>tag:fed,2024:1</id>
<link href="https://www.federalreserve.gov/x"/><updated>2024-03-20T18:00:00Z</updated></entry>
</feed>"#;

    fn headline(published: UnixNanos, title: &str, id: &str) -> Headline {
        Headline {
            published,
            seen: published,
            source: "Test".into(),
            title: title.into(),
            link: format!("https://example.com/{id}"),
            id: id.into(),
            official: false,
        }
    }

    #[test]
    fn rss_reads_with_its_dates_and_tidied_titles() {
        let found = parse_feed(RSS.as_bytes(), "https://example.com/rss", SEEN).unwrap();
        assert_eq!(found.len(), 3, "the empty title is left out");
        assert_eq!(found[0].title, "Bitcoin climbs as traders eye the Fed");
        assert_eq!(found[0].source, "Crypto Wire");
        assert_eq!(found[0].id, "a-1");
        // 2024-03-10 14:30:05 UTC.
        assert_eq!(found[0].published, 1_710_081_005_000_000_000);
        assert_eq!(found[1].published, 1_709_971_200_000_000_000);
        // No date: when it was seen.
        assert_eq!(found[2].published, SEEN);
        assert!(!found[0].official);
    }

    #[test]
    fn atom_reads_and_a_central_banks_feed_is_official() {
        let found = parse_feed(
            ATOM.as_bytes(),
            "https://www.federalreserve.gov/feeds/press_all.xml",
            SEEN,
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "FOMC statement");
        assert_eq!(found[0].published, 1_710_957_600_000_000_000);
        assert!(found[0].official);
        assert_eq!(found[0].importance(&[]), Importance::High);
        assert!(parse_feed(b"<html>not a feed</html>", "x", SEEN).is_err());
    }

    #[test]
    fn importance_is_by_its_words_and_the_market() {
        let words = market_words("BTCUSDT");
        assert_eq!(words, ["BTC", "Bitcoin"]);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let of = |title: &str| headline(0, title, "x").importance(&words);
        assert_eq!(of("Exchange hacked for $40m"), Importance::High);
        assert_eq!(of("US CPI comes in hot"), Importance::High);
        assert_eq!(of("Bitcoin miners sell"), Importance::Medium);
        assert_eq!(of("BTC holds 60k"), Importance::Medium);
        assert_eq!(
            of("WBTC bridge reopens"),
            Importance::Low,
            "a word of its own"
        );
        assert_eq!(of("NFT sales fall"), Importance::Low);
        assert_eq!(market_words("ETHUSDT-PERP"), ["ETH", "Ethereum", "Ether"]);
        assert_eq!(market_words("SOLBTC"), ["SOL", "Solana"]);
    }

    #[test]
    fn the_store_keeps_each_once_in_time_and_never_shows_one_from_later() {
        let dir = std::env::temp_dir().join(format!("persephone-news-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = NewsStore::open(&dir);
        let added = store
            .add(vec![
                headline(30, "Third", "c"),
                headline(10, "First", "a"),
                headline(20, "Second", "b"),
            ])
            .unwrap();
        assert_eq!(added.len(), 3);
        // The same again, by id or by link, isn't added twice.
        let mut again = headline(20, "Second, edited", "b");
        assert!(store.add(vec![again.clone()]).unwrap().is_empty());
        again.id = "other".into();
        assert!(store.add(vec![again]).unwrap().is_empty(), "same link");
        let titles =
            |list: Vec<&Headline>| list.iter().map(|h| h.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(store.until(25, 10)), ["Second", "First"]);
        assert_eq!(titles(store.until(20, 1)), ["Second"]);
        assert!(store.until(5, 10).is_empty());
        assert_eq!(store.between(10, 30).len(), 2);
        assert_eq!(store.since(), Some(10));

        // Read back from disk, the same.
        let back = NewsStore::open(&dir);
        assert_eq!(back.all(), store.all());
        fs::remove_dir_all(dir).unwrap();
    }

    const GDELT_ANSWER: &str = r#"{"articles": [
        {"url": "https://news.example/a", "url_mobile": "", "title": "Bitcoin  slides as  Fed holds",
         "seendate": "20240105T123000Z", "socialimage": "", "domain": "news.example",
         "language": "English", "sourcecountry": "United States"},
        {"url": "https://www.federalreserve.gov/x", "title": "FOMC statement",
         "seendate": "20240131T190000Z", "domain": "federalreserve.gov"},
        {"url": "https://news.example/blank", "title": "", "seendate": "20240105T123000Z"}
    ]}"#;

    #[test]
    fn gdelts_answers_read_as_headlines_at_when_it_saw_them() {
        let found = parse_gdelt(GDELT_ANSWER.as_bytes(), SEEN).unwrap();
        assert_eq!(found.len(), 2, "the blank title is left out");
        assert_eq!(found[0].title, "Bitcoin slides as Fed holds");
        // 2024-01-05 12:30:00 UTC.
        assert_eq!(found[0].published, 1_704_457_800_000_000_000);
        assert_eq!(found[0].source, "news.example");
        assert_eq!(found[0].id, "https://news.example/a");
        assert!(found[1].official && !found[0].official);
        assert!(parse_gdelt(b"{}", SEEN).unwrap().is_empty(), "none found");
        let refused = parse_gdelt(b"The specified phrase is too short.", SEEN).unwrap_err();
        assert!(refused.contains("too short"), "{refused}");
        assert_eq!(gdelt_time(1_704_457_800_000_000_000), "20240105123000");
    }

    #[test]
    fn history_is_searched_in_stretches_and_each_only_once() {
        let words = market_words("BTCUSDT");
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        assert_eq!(market_query(&words).as_deref(), Some("(btc OR bitcoin)"));
        assert_eq!(market_query(&["OP"]), None, "too short for GDELT");
        let day = NANOS_PER_DAY;
        // Thirty days from 2024-01-01: three stretches on the grid, the first starting before.
        let from = 1_704_067_200_000_000_000;
        let stretches = history_stretches(from, from + 30 * day);
        assert_eq!(stretches.len(), 3, "{stretches:?}");
        assert!(stretches[0].0 <= from && stretches[0].1 > from);
        assert_eq!((stretches[0].0 - GDELT_FROM) % (HISTORY_DAYS * day), 0);
        assert!(
            history_stretches(0, GDELT_FROM).is_empty(),
            "nothing before 2017"
        );

        let dir = std::env::temp_dir().join(format!("persephone-searched-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = NewsStore::open(&dir);
        let left = store.searches_left(Some("bitcoin"), from, from + 30 * day);
        assert_eq!(
            left.len(),
            6,
            "the market's and the economy's, each stretch"
        );
        store.mark_searched(left[0].key.clone()).unwrap();
        let back = NewsStore::open(&dir);
        assert_eq!(
            back.searches_left(Some("bitcoin"), from, from + 30 * day)
                .len(),
            5
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn feeds_come_from_a_file_or_are_the_defaults() {
        let dir = std::env::temp_dir().join(format!("persephone-feeds-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(feeds(&dir).len(), DEFAULT_FEEDS.len());
        fs::create_dir_all(dir.join("news")).unwrap();
        fs::write(
            dir.join("news").join("feeds.txt"),
            "# mine\nhttps://a.example/rss\n\n https://b.example/atom \n",
        )
        .unwrap();
        assert_eq!(
            feeds(&dir),
            ["https://a.example/rss", "https://b.example/atom"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

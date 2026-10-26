//! Working out a CSV file's layout from its first lines.
//!
//! Header names decide when they're recognisable (`time`, `price`, `qty`, `Bid`,
//! `is_buyer_maker`, ...). Otherwise the values do: the time column is the one that reads as
//! timestamps in order, two close columns with one always at or above the other are a bid and
//! an ask, steadily increasing integers are ids, and so on. The result is checked by parsing
//! the sample with it.

use std::{fmt::Write as _, str::FromStr};

use anyhow::{Context, bail, ensure};
use csv::ByteRecord;
use persephone_core::tick::TickKind;

use super::{
    Columns, Format, RowParser, SideFormat, Tick,
    time::{self, TimeFormat, Zone},
};

/// What [`detect`] found, with enough context to show someone.
#[derive(Clone, Debug)]
pub struct Detection {
    pub format: Format,
    /// Column names from the header, or `column 1`, `column 2`, ... without one.
    pub names: Vec<String>,
    /// Each column's value in the first data row.
    pub example: Vec<String>,
    /// The most decimals seen in prices and in sizes.
    pub price_decimals: u8,
    pub size_decimals: u8,
    /// The first few rows, parsed.
    pub preview: Vec<Tick>,
    /// Guesses worth a second look.
    pub notes: Vec<String>,
    /// The times don't say their zone and none was given, so they're read as UTC.
    pub zone_assumed: bool,
    /// Data rows in the sample, and how many of them didn't parse.
    pub rows: usize,
    pub bad_rows: usize,
}

/// Choices that win over detection.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub delimiter: Option<char>,
    pub header: Option<bool>,
    pub kind: Option<TickKind>,
    pub time_format: Option<TimeFormat>,
    pub zone: Option<Zone>,
    /// A role and its column, by header name or number from 1.
    pub columns: Vec<(Role, String)>,
}

/// What a column can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Time,
    Date,
    Price,
    Size,
    Side,
    Bid,
    Ask,
    BidSize,
    AskSize,
}

impl Role {
    pub const ALL: [Self; 9] = [
        Self::Time,
        Self::Date,
        Self::Price,
        Self::Size,
        Self::Side,
        Self::Bid,
        Self::Ask,
        Self::BidSize,
        Self::AskSize,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Date => "date",
            Self::Price => "price",
            Self::Size => "size",
            Self::Side => "side",
            Self::Bid => "bid",
            Self::Ask => "ask",
            Self::BidSize => "bid size",
            Self::AskSize => "ask size",
        }
    }

    /// Normalised header names that mean this role.
    const fn names(self) -> &'static [&'static str] {
        match self {
            Self::Time => &[
                "time",
                "timestamp",
                "ts",
                "datetime",
                "gmttime",
                "localtime",
                "utctime",
                "t",
                "transacttime",
                "tradetime",
                "ratedatetime",
                "timeutc",
                "date",
            ],
            Self::Date => &["date", "day", "tradedate"],
            Self::Price => &[
                "price",
                "last",
                "lastprice",
                "px",
                "tradeprice",
                "rate",
                "p",
                "close",
            ],
            Self::Size => &[
                "size",
                "qty",
                "quantity",
                "amount",
                "volume",
                "vol",
                "lastsize",
                "q",
                "tradesize",
                "baseqty",
                "basevolume",
                "lastqty",
                "v",
            ],
            Self::Side => &[
                "side",
                "aggressor",
                "aggressorside",
                "takerside",
                "direction",
                "isbuyermaker",
                "buyermaker",
                "buyerismaker",
                "m",
            ],
            Self::Bid => &["bid", "bidprice", "ratebid", "bestbid", "bidpx", "bidrate"],
            Self::Ask => &[
                "ask",
                "askprice",
                "rateask",
                "bestask",
                "askpx",
                "offer",
                "offerprice",
                "askrate",
            ],
            Self::BidSize => &[
                "bidsize",
                "bidvolume",
                "bidqty",
                "bidvol",
                "bidamount",
                "bidsz",
                "bidquantity",
            ],
            Self::AskSize => &[
                "asksize",
                "askvolume",
                "askqty",
                "askvol",
                "askamount",
                "asksz",
                "askquantity",
                "offersize",
                "offervolume",
            ],
        }
    }
}

impl FromStr for Role {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let wanted = normalise(s);
        Self::ALL
            .into_iter()
            .find(|role| normalise(role.name()) == wanted)
            .with_context(|| format!("`{s}` isn't a column role (time, date, price, size, side, bid, ask, bid-size, ask-size)"))
    }
}

/// How many sample rows the column checks look at.
const ROWS_CHECKED: usize = 1_000;

/// Works out the layout of a CSV file from its start (see [`sample`](super::sample)).
///
/// # Errors
///
/// Returns a message saying what couldn't be worked out, and which option settles it.
pub fn detect(sample: &str, overrides: &Overrides) -> anyhow::Result<Detection> {
    let delimiter = match overrides.delimiter {
        Some(d) => d,
        None => sniff_delimiter(sample)?,
    };
    let rows = split(sample, delimiter)?;
    ensure!(!rows.is_empty(), "the file has no rows");
    let header = overrides.header.unwrap_or_else(|| looks_like_header(&rows));
    let data = if header { &rows[1..] } else { &rows[..] };
    ensure!(!data.is_empty(), "there are no rows after the header");
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let names: Vec<String> = (0..width)
        .map(|i| match rows[0].get(i) {
            Some(name) if header => name.clone(),
            _ => format!("column {}", i + 1),
        })
        .collect();
    let checked = &data[..data.len().min(ROWS_CHECKED)];
    let decimal_comma = delimiter != ',' && uses_decimal_comma(checked);
    let classes: Vec<Class> = (0..width)
        .map(|i| classify(checked, i, decimal_comma))
        .collect();
    let mut notes = Vec::new();

    // Columns named on the command line win; the rest are found by name, then by value.
    let mut forced = Columns::default();
    let mut forced_roles = Vec::new();
    for (role, spec) in &overrides.columns {
        let column = resolve(spec, &names, header)?;
        set(&mut forced, *role, column);
        forced_roles.push(*role);
    }
    let is_forced = |role| forced_roles.contains(&role);

    let (time_column, date_column, mut time_format) = if is_forced(Role::Time) {
        let date = is_forced(Role::Date).then_some(forced.date).flatten();
        let format = match (&classes[forced.time], date) {
            (_, Some(date)) => classes[date].time_format(),
            (class, None) => class.time_format(),
        };
        (forced.time, date, format)
    } else {
        find_time(&classes, &names, header).context(
            "couldn't find the time column; name it with --time (a header name or column number)",
        )?
    };
    if let Some(format) = overrides.time_format {
        time_format = Some(format);
    }
    let time_format = time_format.context(
        "couldn't tell how the time is written; say so with --time-format (e.g. epoch-millis or year-first)",
    )?;
    if let Some(note) = classes[date_column.unwrap_or(time_column)].ambiguity() {
        notes.push(note);
    }

    let mut columns = Columns {
        time: time_column,
        date: date_column,
        ..Columns::default()
    };
    let used = |columns: &Columns, i: usize| columns.time == i || columns.date == Some(i);

    // By header name.
    if header {
        for role in [
            Role::Price,
            Role::Size,
            Role::Side,
            Role::Bid,
            Role::Ask,
            Role::BidSize,
            Role::AskSize,
        ] {
            let found = (0..width).find(|&i| {
                !used(&columns, i)
                    && !assigned(&columns, i)
                    && role.names().contains(&normalise(&names[i]).as_str())
                    && classes[i].fits(role)
            });
            if let Some(i) = found {
                set(&mut columns, role, i);
                if role == Role::Side {
                    columns.side_format = side_format(&classes[i], &names[i]);
                }
            }
        }
    }

    // By value, for whatever names didn't settle.
    let has_quotes = columns.bid.is_some() && columns.ask.is_some();
    if !has_quotes && columns.price.is_none() {
        let numbers: Vec<usize> = (0..width)
            .filter(|&i| !used(&columns, i) && !assigned(&columns, i))
            .filter(|&i| matches!(classes[i], Class::Number { id: false, .. }))
            .collect();
        if let Some((bid, ask)) = bid_ask(checked, &numbers, decimal_comma) {
            columns.bid = Some(bid);
            columns.ask = Some(ask);
            let rest: Vec<usize> = numbers
                .iter()
                .copied()
                .filter(|&i| i != bid && i != ask)
                .collect();
            if let [first, second, ..] = rest[..] {
                // Sizes follow the prices' order.
                let (bid_size, ask_size) = if bid < ask {
                    (first, second)
                } else {
                    (second, first)
                };
                columns.bid_size = Some(bid_size);
                columns.ask_size = Some(ask_size);
            }
        } else if let Some(&price) = numbers.first() {
            columns.price = Some(price);
            if columns.size.is_none() {
                columns.size = numbers.get(1).copied();
            }
        }
    }
    if columns.side.is_none() {
        let side = (0..width).find(|&i| {
            !used(&columns, i)
                && !assigned(&columns, i)
                && matches!(classes[i], Class::Bool | Class::SideWord)
        });
        if let Some(i) = side {
            columns.side = Some(i);
            columns.side_format = side_format(&classes[i], &names[i]);
        }
    }

    for role in &forced_roles {
        let column = get(&forced, *role).expect("forced roles are set");
        set(&mut columns, *role, column);
        if *role == Role::Side {
            columns.side_format = side_format(&classes[column], &names[column]);
        }
    }

    let kind = match overrides.kind {
        Some(kind) => kind,
        None if columns.bid.is_some() && columns.ask.is_some() => TickKind::Quotes,
        None if columns.price.is_some() => TickKind::Trades,
        None => bail!(
            "couldn't find prices; name the column with --price (or --bid and --ask for quotes)"
        ),
    };
    match kind {
        TickKind::Trades => {
            ensure!(
                columns.price.is_some(),
                "couldn't find the price column; name it with --price"
            );
            (columns.bid, columns.ask, columns.bid_size, columns.ask_size) =
                (None, None, None, None);
        }
        TickKind::Quotes => {
            ensure!(
                columns.bid.is_some() && columns.ask.is_some(),
                "couldn't find bid and ask columns; name them with --bid and --ask"
            );
            (columns.price, columns.size, columns.side) = (None, None, None);
        }
    }

    let explicit_zone = overrides.zone.is_some();
    let format = Format {
        delimiter,
        header,
        decimal_comma,
        kind,
        columns,
        time_format,
        zone: overrides.zone.clone().unwrap_or_default(),
    };
    let zone_assumed =
        !time_format.is_epoch() && !explicit_zone && !writes_offsets(checked, &columns);
    let mut detection = check(format, names, data, notes)?;
    detection.zone_assumed = zone_assumed;
    Ok(detection)
}

/// Parses the sample with `format` to make sure it fits, and collects what to show.
fn check(
    format: Format,
    names: Vec<String>,
    data: &[Vec<String>],
    notes: Vec<String>,
) -> anyhow::Result<Detection> {
    let mut parser = RowParser::new(&format);
    let (mut preview, mut bad, mut first_problem) = (Vec::new(), 0, None);
    let (mut price_decimals, mut size_decimals) = (0, 0);
    for (i, row) in data.iter().enumerate() {
        match parser.parse(&ByteRecord::from(row.clone())) {
            Ok(parsed) => {
                price_decimals = price_decimals.max(parsed.price_decimals);
                size_decimals = size_decimals.max(parsed.size_decimals);
                if preview.len() < 3 {
                    preview.push(parsed.tick);
                }
            }
            Err(reason) => {
                bad += 1;
                first_problem.get_or_insert_with(|| format!("data row {}: {reason}", i + 1));
            }
        }
    }
    let rows = data.len();
    if bad * 10 > rows {
        bail!(
            "{bad} of the first {rows} rows don't fit the layout found ({}); {}",
            first_problem.unwrap_or_default(),
            "name the columns with --time, --price, --size, --bid, --ask"
        );
    }
    Ok(Detection {
        example: data[0].clone(),
        format,
        names,
        price_decimals,
        size_decimals,
        preview,
        notes,
        zone_assumed: false,
        rows,
        bad_rows: bad,
    })
}

impl Detection {
    /// The separator, header, kind and how times read, in one line.
    #[must_use]
    pub fn summary(&self) -> String {
        let format = &self.format;
        let separator = match format.delimiter {
            ',' => "comma-separated".to_owned(),
            ';' => "semicolon-separated".to_owned(),
            '\t' => "tab-separated".to_owned(),
            '|' => "pipe-separated".to_owned(),
            other => format!("separated by `{other}`"),
        };
        let header = if format.header {
            "with a header row"
        } else {
            "no header row"
        };
        let comma = if format.decimal_comma {
            ", decimal commas"
        } else {
            ""
        };
        let zone = if format.time_format.is_epoch() {
            String::new()
        } else {
            format!(", {}", format.zone)
        };
        format!(
            "{separator}, {header}, {}{comma}; times are {}{zone}",
            format.kind,
            format.time_format.describe()
        )
    }

    /// Each role found and its column (from 0), date and time first.
    #[must_use]
    pub fn roles(&self) -> Vec<(Role, usize)> {
        let c = &self.format.columns;
        [
            (Role::Date, c.date),
            (Role::Time, Some(c.time)),
            (Role::Price, c.price),
            (Role::Size, c.size),
            (Role::Side, c.side),
            (Role::Bid, c.bid),
            (Role::Ask, c.ask),
            (Role::BidSize, c.bid_size),
            (Role::AskSize, c.ask_size),
        ]
        .into_iter()
        .filter_map(|(role, column)| Some((role, column?)))
        .collect()
    }

    /// What `side` values mean, when there's a side column.
    #[must_use]
    pub fn side_meaning(&self) -> Option<&'static str> {
        self.format.columns.side?;
        Some(match self.format.columns.side_format {
            SideFormat::Words => "buy / sell",
            SideFormat::BuyerIsMaker => "true = the seller took liquidity",
            SideFormat::Sign => "1 buy, -1 sell",
        })
    }

    /// A few lines saying what was found, for people.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = self.summary();
        out.push('\n');
        for (role, i) in self.roles() {
            let name = self.names.get(i).map_or("", String::as_str);
            let example = self.example.get(i).map_or("", String::as_str);
            let note = match (role, self.side_meaning()) {
                (Role::Side, Some(meaning)) => format!("  ({meaning})"),
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "  {:<9} column {:<3} {:<18} {example}{note}",
                role.name(),
                i + 1,
                format!("\"{name}\""),
            );
        }
        out
    }
}

// --- Rows ------------------------------------------------------------------------------------

fn split(sample: &str, delimiter: char) -> anyhow::Result<Vec<Vec<String>>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(u8::try_from(delimiter).context("the separator must be one ASCII character")?)
        .has_headers(false)
        .flexible(true)
        .from_reader(sample.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records().take(2 * ROWS_CHECKED) {
        let Ok(record) = record else { continue };
        let row: Vec<String> = record.iter().map(|f| f.trim().to_owned()).collect();
        if row.iter().any(|f| !f.is_empty()) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The separator that splits most lines into the same number (two or more) of fields.
fn sniff_delimiter(sample: &str) -> anyhow::Result<char> {
    let mut best: Option<(char, usize)> = None;
    for candidate in [',', ';', '\t', '|'] {
        let Ok(rows) = split(sample, candidate) else {
            continue;
        };
        let mut counts = std::collections::HashMap::new();
        for row in &rows {
            *counts.entry(row.len()).or_insert(0usize) += 1;
        }
        let Some((&fields, &lines)) = counts
            .iter()
            .max_by_key(|(fields, lines)| (**lines, **fields))
        else {
            continue;
        };
        if fields >= 2 && lines * 10 >= rows.len() * 9 && best.is_none_or(|(_, f)| fields > f) {
            best = Some((candidate, fields));
        }
    }
    best.map(|(d, _)| d)
        .context("couldn't tell what separates the columns; say with --delimiter")
}

/// A header when some column starts with a word and continues with numbers or times.
fn looks_like_header(rows: &[Vec<String>]) -> bool {
    if rows.len() < 2 {
        return rows
            .first()
            .is_some_and(|row| row.iter().all(|f| cell(f) == Cell::Word));
    }
    let rest = &rows[1..rows.len().min(ROWS_CHECKED)];
    rows[0].iter().enumerate().any(|(i, first)| {
        cell(first) == Cell::Word && {
            let values = rest
                .iter()
                .filter_map(|row| row.get(i))
                .filter(|v| !v.is_empty());
            let (mut total, mut data) = (0, 0);
            for v in values {
                total += 1;
                data += usize::from(matches!(cell(v), Cell::Value));
            }
            total > 0 && data * 10 >= total * 9
        }
    })
}

#[derive(PartialEq)]
enum Cell {
    Value,
    Flag,
    Word,
}

fn cell(text: &str) -> Cell {
    if text.parse::<f64>().is_ok()
        || text.replace(',', ".").parse::<f64>().is_ok()
        || is_time_of_day(text)
        || [
            TimeFormat::YearFirst,
            TimeFormat::DayFirst,
            TimeFormat::MonthFirst,
        ]
        .iter()
        .any(|&f| time::calendar(text, f).is_ok())
    {
        Cell::Value
    } else if is_bool(text) || is_side_word(text) {
        Cell::Flag
    } else {
        Cell::Word
    }
}

fn uses_decimal_comma(rows: &[Vec<String>]) -> bool {
    let mut comma = 0;
    for value in rows.iter().flatten() {
        if value.contains('.') && value.parse::<f64>().is_ok() {
            return false;
        }
        if value.contains(',') && value.replace(',', ".").parse::<f64>().is_ok() {
            comma += 1;
        }
    }
    comma > 0
}

// --- Columns ---------------------------------------------------------------------------------

/// What a column's values look like.
#[derive(Clone, Debug, PartialEq)]
enum Class {
    Empty,
    /// Numbers; `id` when they're integers that rise on every row.
    Number {
        integer: bool,
        id: bool,
    },
    /// Epoch numbers; `ordered` when they mostly rise.
    Epoch {
        format: TimeFormat,
        ordered: bool,
    },
    /// Calendar text; `ambiguous` when day and month could be either way round.
    Calendar {
        format: TimeFormat,
        date_only: bool,
        ambiguous: bool,
    },
    /// `HH:MM:SS(.fff)` on its own.
    TimeOfDay,
    Bool,
    SideWord,
    Text,
}

impl Class {
    fn time_format(&self) -> Option<TimeFormat> {
        match self {
            Self::Epoch { format, .. } | Self::Calendar { format, .. } => Some(*format),
            Self::Number { .. } => None,
            _ => None,
        }
    }

    fn ambiguity(&self) -> Option<String> {
        match self {
            Self::Calendar {
                format,
                ambiguous: true,
                ..
            } => Some(format!(
                "every day in the sample is 12 or less, so the dates could be read either way; assumed {} (use --time-format day-first or month-first)",
                if *format == TimeFormat::DayFirst {
                    "day first"
                } else {
                    "month first"
                }
            )),
            _ => None,
        }
    }

    fn fits(&self, role: Role) -> bool {
        match role {
            Role::Side => matches!(
                self,
                Self::Bool | Self::SideWord | Self::Number { integer: true, .. }
            ),
            Role::Time | Role::Date => true,
            _ => matches!(self, Self::Number { .. }),
        }
    }
}

fn classify(rows: &[Vec<String>], column: usize, decimal_comma: bool) -> Class {
    let values: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.get(column))
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .collect();
    if values.is_empty() {
        return Class::Empty;
    }
    if mostly(&values, is_bool) {
        return Class::Bool;
    }
    if mostly(&values, is_side_word) {
        return Class::SideWord;
    }
    if mostly(&values, is_time_of_day) {
        return Class::TimeOfDay;
    }
    if let Some(class) = calendar_class(&values) {
        return class;
    }
    if !mostly(&values, |v| number(v, decimal_comma).is_some()) {
        return Class::Text;
    }

    let numbers: Vec<f64> = values
        .iter()
        .filter_map(|v| number(v, decimal_comma))
        .collect();
    let integer = values
        .iter()
        .filter(|v| number(v, decimal_comma).is_some())
        .all(|v| v.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
    let units: Vec<Option<TimeFormat>> = numbers.iter().map(|&n| time::epoch_unit(n)).collect();
    if let Some(Some(unit)) = units.first()
        && units.iter().filter(|u| **u == Some(*unit)).count() * 20 >= units.len() * 19
    {
        return Class::Epoch {
            format: *unit,
            ordered: in_order(&numbers),
        };
    }
    let id = integer && numbers.len() > 2 && numbers.windows(2).all(|w| w[1] > w[0]);
    Class::Number { integer, id }
}

/// Whether at least 95% of `values` pass `test`: a stray bad row shouldn't change what a
/// column is.
fn mostly(values: &[&str], test: impl Fn(&str) -> bool) -> bool {
    let fit = values.iter().filter(|v| test(v)).count();
    fit * 20 >= values.len() * 19
}

fn calendar_class(values: &[&str]) -> Option<Class> {
    let fits = |format| mostly(values, |v| time::calendar(v, format).is_ok());
    let date_only = mostly(values, |v| {
        !v.contains(':')
            && [
                TimeFormat::YearFirst,
                TimeFormat::DayFirst,
                TimeFormat::MonthFirst,
            ]
            .iter()
            .any(|&f| time::calendar(v, f).is_ok_and(|(_, nanos, _)| nanos == 0))
    });
    if fits(TimeFormat::YearFirst) {
        return Some(Class::Calendar {
            format: TimeFormat::YearFirst,
            date_only,
            ambiguous: false,
        });
    }
    let (day_first, month_first) = (fits(TimeFormat::DayFirst), fits(TimeFormat::MonthFirst));
    let format = match (day_first, month_first) {
        (true, false) => TimeFormat::DayFirst,
        (false, true) => TimeFormat::MonthFirst,
        // `01/02/2014` is usually American; `01.02.2014` and `01-02-2014` European.
        (true, true) if values[0].contains('/') => TimeFormat::MonthFirst,
        (true, true) => TimeFormat::DayFirst,
        (false, false) => return None,
    };
    Some(Class::Calendar {
        format,
        date_only,
        ambiguous: day_first && month_first,
    })
}

/// Picks the time column: by name first, then calendar text, then a date column followed by a
/// time-of-day column, then the finest-grained epoch column.
fn find_time(
    classes: &[Class],
    names: &[String],
    header: bool,
) -> Option<(usize, Option<usize>, Option<TimeFormat>)> {
    let timestamp = |i: usize| match &classes[i] {
        Class::Epoch { format, .. } => Some(*format),
        Class::Calendar {
            format,
            date_only: false,
            ..
        } => Some(*format),
        _ => None,
    };
    let date_then_time = |i: usize| match &classes[i] {
        Class::Calendar {
            format,
            date_only: true,
            ..
        } => (0..classes.len())
            .find(|&j| j != i && classes[j] == Class::TimeOfDay)
            .map(|time| (time, Some(i), Some(*format))),
        _ => None,
    };

    if header {
        let named = |role: Role| {
            (0..classes.len())
                .filter(move |&i| role.names().contains(&normalise(&names[i]).as_str()))
        };
        if let Some(found) =
            named(Role::Time).find_map(|i| timestamp(i).map(|f| (i, None, Some(f))))
        {
            return Some(found);
        }
        if let Some(found) = named(Role::Date).find_map(date_then_time) {
            return Some(found);
        }
    }
    if let Some(i) = (0..classes.len()).find(|&i| {
        matches!(
            classes[i],
            Class::Calendar {
                date_only: false,
                ..
            }
        )
    }) {
        return Some((i, None, timestamp(i)));
    }
    if let Some(found) = (0..classes.len()).find_map(date_then_time) {
        return Some(found);
    }
    // Finer units first (a column of trade ids can look like epoch seconds), then columns in
    // order.
    let rank = |format: TimeFormat| match format {
        TimeFormat::EpochNanos => 0,
        TimeFormat::EpochMicros => 1,
        TimeFormat::EpochMillis => 2,
        _ => 3,
    };
    (0..classes.len())
        .filter_map(|i| match classes[i] {
            Class::Epoch { format, ordered } => Some(((rank(format), !ordered, i), format)),
            _ => None,
        })
        .min_by_key(|&(key, _)| key)
        .map(|((_, _, i), format)| (i, None, Some(format)))
}

/// Two columns that are close (within 2%) with one at or above the other on almost every row:
/// a bid and an ask, returned as (bid, ask).
fn bid_ask(rows: &[Vec<String>], numbers: &[usize], decimal_comma: bool) -> Option<(usize, usize)> {
    let values = |i: usize| -> Vec<f64> {
        rows.iter()
            .filter_map(|row| row.get(i).and_then(|v| number(v, decimal_comma)))
            .collect()
    };
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    // Neighbours first: bid and ask usually sit side by side.
    for w in numbers.windows(2) {
        pairs.push((w[0], w[1]));
    }
    for (n, &a) in numbers.iter().enumerate() {
        for &b in numbers.iter().skip(n + 2) {
            pairs.push((a, b));
        }
    }
    pairs.into_iter().find_map(|(a, b)| {
        let (va, vb) = (values(a), values(b));
        if va.len() != vb.len() || va.is_empty() || va.iter().chain(&vb).any(|v| *v <= 0.0) {
            return None;
        }
        let n = va.len();
        let below = va.iter().zip(&vb).filter(|(x, y)| x <= y).count();
        let above = va.iter().zip(&vb).filter(|(x, y)| x >= y).count();
        let differ = va.iter().zip(&vb).filter(|(x, y)| x != y).count();
        let close = va
            .iter()
            .zip(&vb)
            .all(|(x, y)| (x - y).abs() / x.max(*y) < 0.02);
        if !close || differ * 2 < n {
            return None;
        }
        if below * 50 >= n * 49 {
            Some((a, b))
        } else if above * 50 >= n * 49 {
            Some((b, a))
        } else {
            None
        }
    })
}

fn side_format(class: &Class, name: &str) -> SideFormat {
    let name = normalise(name);
    match class {
        Class::Number { .. } => SideFormat::Sign,
        Class::Bool => SideFormat::BuyerIsMaker,
        _ if name.contains("maker") || name == "m" => SideFormat::BuyerIsMaker,
        _ => SideFormat::Words,
    }
}

/// Whether the sample's timestamps carry an offset or `Z`.
fn writes_offsets(rows: &[Vec<String>], columns: &Columns) -> bool {
    rows.iter()
        .filter_map(|row| row.get(columns.time))
        .any(|v| {
            let v = v.trim_end();
            v.ends_with('Z') || v.ends_with("UTC") || v.ends_with("GMT") || {
                let tail = &v[v.len().saturating_sub(6)..];
                tail.contains('+')
                    || (tail.contains('-') && v.contains(':') && v.rfind('-') > v.find(':'))
            }
        })
}

fn in_order(values: &[f64]) -> bool {
    let pairs = values.len().saturating_sub(1);
    let ordered = values.windows(2).filter(|w| w[1] >= w[0]).count();
    pairs == 0 || ordered * 20 >= pairs * 19
}

fn number(text: &str, decimal_comma: bool) -> Option<f64> {
    let value = if decimal_comma {
        text.replace(',', ".").parse()
    } else {
        text.parse()
    };
    value.ok().filter(|v: &f64| v.is_finite())
}

fn is_bool(text: &str) -> bool {
    ["true", "false"]
        .iter()
        .any(|w| text.eq_ignore_ascii_case(w))
}

fn is_side_word(text: &str) -> bool {
    ["buy", "sell", "b", "s"]
        .iter()
        .any(|w| text.eq_ignore_ascii_case(w))
}

fn is_time_of_day(text: &str) -> bool {
    let mut parts = text.splitn(3, ':');
    let (Some(h), Some(m)) = (parts.next(), parts.next()) else {
        return false;
    };
    let digits = |s: &str, lens: std::ops::RangeInclusive<usize>| {
        lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    let seconds_ok = parts.next().is_none_or(|s| {
        let whole = s.split(['.', ',']).next().unwrap_or("");
        digits(whole, 2..=2) && s[whole.len()..].bytes().skip(1).all(|b| b.is_ascii_digit())
    });
    digits(h, 1..=2) && digits(m, 2..=2) && seconds_ok
}

/// Lowercase letters and digits only: `Bid Volume` and `bid_volume` both become `bidvolume`.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A column given as a header name or a number from 1.
fn resolve(spec: &str, names: &[String], header: bool) -> anyhow::Result<usize> {
    if let Ok(n) = spec.parse::<usize>() {
        ensure!(
            n >= 1 && n <= names.len(),
            "there's no column {n} (the file has {})",
            names.len()
        );
        return Ok(n - 1);
    }
    ensure!(header, "the file has no header, so name columns by number");
    let wanted = normalise(spec);
    names
        .iter()
        .position(|name| normalise(name) == wanted)
        .with_context(|| format!("there's no column called `{spec}`"))
}

fn get(columns: &Columns, role: Role) -> Option<usize> {
    match role {
        Role::Time => Some(columns.time),
        Role::Date => columns.date,
        Role::Price => columns.price,
        Role::Size => columns.size,
        Role::Side => columns.side,
        Role::Bid => columns.bid,
        Role::Ask => columns.ask,
        Role::BidSize => columns.bid_size,
        Role::AskSize => columns.ask_size,
    }
}

fn set(columns: &mut Columns, role: Role, column: usize) {
    match role {
        Role::Time => columns.time = column,
        Role::Date => columns.date = Some(column),
        Role::Price => columns.price = Some(column),
        Role::Size => columns.size = Some(column),
        Role::Side => columns.side = Some(column),
        Role::Bid => columns.bid = Some(column),
        Role::Ask => columns.ask = Some(column),
        Role::BidSize => columns.bid_size = Some(column),
        Role::AskSize => columns.ask_size = Some(column),
    }
}

/// Whether a value role already has `column`.
fn assigned(columns: &Columns, column: usize) -> bool {
    [
        columns.price,
        columns.size,
        columns.side,
        columns.bid,
        columns.ask,
        columns.bid_size,
        columns.ask_size,
    ]
    .contains(&Some(column))
}

use std::io::Write as _;

use persephone_core::{Timeframe, tick::Aggressor, time::NANOS_PER_DAY};

use super::*;

/// 2014-01-02 03:04:05 UTC.
const T: UnixNanos = 1_388_631_845_000_000_000;
const MS: UnixNanos = 1_000_000;

fn found(sample: &str) -> Detection {
    detect(sample, &Overrides::default()).unwrap_or_else(|e| panic!("{e:#}\n{sample}"))
}

fn trade(d: &Detection, i: usize) -> Trade {
    match d.preview[i] {
        Tick::Trade(t) => t,
        _ => panic!("expected a trade"),
    }
}

fn quote(d: &Detection, i: usize) -> Quote {
    match d.preview[i] {
        Tick::Quote(q) => q,
        _ => panic!("expected a quote"),
    }
}

fn bar(d: &Detection, i: usize) -> Candle {
    match d.preview[i] {
        Tick::Bar(b) => b,
        _ => panic!("expected a bar"),
    }
}

#[test]
fn bitcoincharts_dumps() {
    let d = found(
        "1388631845,5.800000000000,1.000000000000\n1388631846,5.810000000000,0.250000000000\n1388631846,5.79,2\n",
    );
    assert_eq!(d.format.kind, Kind::Trades);
    assert!(!d.format.header);
    assert_eq!(d.format.time_format, TimeFormat::EpochSeconds);
    assert_eq!(
        trade(&d, 0),
        Trade {
            ts: T,
            price: 5.8,
            size: 1.0,
            aggressor: Aggressor::Unknown
        }
    );
    assert_eq!(
        (d.price_decimals, d.size_decimals),
        (2, 2),
        "trailing zeros don't count"
    );
}

#[test]
fn binance_trade_archives_without_a_header() {
    // id, price, qty, quote_qty, time, is_buyer_maker, is_best_match
    let d = found(
        "1500000000,42000.10,0.0015,63.00015,1388631845000,True,True\n\
         1500000001,42000.20,0.2,8400.04,1388631845001,False,True\n\
         1500000002,42000.00,0.01,420.0,1388631845002,True,True\n",
    );
    let c = d.format.columns;
    assert_eq!(
        (c.time, c.price, c.size, c.side),
        (4, Some(1), Some(2), Some(5)),
        "ids and quote_qty skipped"
    );
    assert_eq!(
        d.format.time_format,
        TimeFormat::EpochMillis,
        "the id column looks like seconds; millis win"
    );
    assert_eq!(trade(&d, 0).aggressor, Aggressor::Seller, "buyer was maker");
    assert_eq!(
        trade(&d, 1),
        Trade {
            ts: T + MS,
            price: 42000.2,
            size: 0.2,
            aggressor: Aggressor::Buyer
        }
    );
}

#[test]
fn binance_agg_trades_with_a_header() {
    let d = found(
        "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n\
         26129,0.01633102,4.70443515,27781,27781,1388631845000000,true\n\
         26130,0.01633103,1.0,27782,27783,1388631845000500,false\n",
    );
    let c = d.format.columns;
    assert!(d.format.header);
    assert_eq!(
        (c.time, c.price, c.size, c.side),
        (5, Some(1), Some(2), Some(6))
    );
    assert_eq!(d.format.time_format, TimeFormat::EpochMicros);
    assert_eq!(c.side_format, SideFormat::BuyerIsMaker);
    assert_eq!(d.price_decimals, 8);
}

#[test]
fn histdata_ticks_in_new_york_standard_time() {
    let sample = "20140101 220405000,1.376250,1.376900,0\n20140101 220405447,1.376260,1.376910,0\n";
    let overrides = Overrides {
        zone: Some("-05:00".parse().unwrap()),
        ..Overrides::default()
    };
    let d = detect(sample, &overrides).unwrap();
    assert_eq!(d.format.kind, Kind::Quotes);
    assert_eq!(d.format.time_format, TimeFormat::YearFirst);
    let q = quote(&d, 0);
    assert_eq!((q.ts, q.bid, q.ask), (T, 1.37625, 1.3769));
    assert_eq!(quote(&d, 1).ts, T + 447 * MS);
    assert!(
        d.notes.is_empty() && !d.zone_assumed,
        "the zone was given: {:?}",
        d.notes
    );
}

#[test]
fn truefx_with_a_symbol_column() {
    let d = found(
        "EUR/USD,20140102 03:04:05.378,1.37624,1.37698\nEUR/USD,20140102 03:04:05.912,1.37625,1.37699\n",
    );
    let c = d.format.columns;
    assert_eq!((c.time, c.bid, c.ask), (1, Some(2), Some(3)));
    assert_eq!(quote(&d, 0).ts, T + 378 * MS);
    assert_eq!(d.price_decimals, 5);
    assert!(d.zone_assumed, "says it assumed UTC");
}

#[test]
fn dukascopy_exports_ask_first_and_day_first() {
    let d = found(
        "Gmt time,Ask,Bid,AskVolume,BidVolume\n\
         02.01.2014 03:04:05.000,1.37698,1.37624,1.5,2.25\n\
         02.01.2014 03:04:05.120,1.37699,1.37625,0.75,1.13\n\
         13.01.2014 03:04:05.120,1.37699,1.37625,0.75,1.13\n",
    );
    let c = d.format.columns;
    assert_eq!(
        (c.bid, c.ask, c.bid_size, c.ask_size),
        (Some(2), Some(1), Some(4), Some(3))
    );
    assert_eq!(d.format.time_format, TimeFormat::DayFirst);
    assert!(
        d.notes.iter().all(|n| !n.contains("either way")),
        "13.01 settles it"
    );
    let q = quote(&d, 0);
    assert_eq!(
        (q.ts, q.bid, q.ask, q.bid_size, q.ask_size),
        (T, 1.37624, 1.37698, 2.25, 1.5)
    );
}

#[test]
fn separate_date_and_time_columns() {
    let d =
        found("2014.01.02,03:04:05.123,1.37624,1.37698\n2014.01.02,03:04:06.000,1.37625,1.37699\n");
    let c = d.format.columns;
    assert_eq!(
        (c.date, c.time, c.bid, c.ask),
        (Some(0), 1, Some(2), Some(3))
    );
    assert_eq!(quote(&d, 0).ts, T + 123 * MS);
}

#[test]
fn gaincapital_with_extra_columns() {
    let d = found(
        "lTid,cDealable,CurrencyPair,RateDateTime,RateBid,RateAsk\n\
         1,D,EUR/USD,2014-01-02 03:04:05.000000000,1.37624,1.37698\n\
         2,D,EUR/USD,2014-01-02 03:04:06.000000000,1.37625,1.37699\n",
    );
    let c = d.format.columns;
    assert_eq!((c.time, c.bid, c.ask), (3, Some(4), Some(5)));
    assert_eq!(quote(&d, 0).ts, T);
}

#[test]
fn kraken_with_side_letters() {
    let d = found(
        "5000.1,0.5,1388631845.1234,b,m,\n5000.2,0.25,1388631846.5,s,l,\n5000.0,1,1388631847,b,l,\n",
    );
    let c = d.format.columns;
    assert_eq!(
        (c.price, c.size, c.time, c.side),
        (Some(0), Some(1), 2, Some(3))
    );
    assert_eq!(c.side_format, SideFormat::Words);
    let t = trade(&d, 0);
    assert_eq!((t.ts, t.aggressor), (T + 123_400_000, Aggressor::Buyer));
    assert_eq!(trade(&d, 1).aggressor, Aggressor::Seller);
}

#[test]
fn semicolons_and_decimal_commas() {
    let d = found(
        "Time;Price;Volume\n2014-01-02 03:04:05;1,3762;100\n2014-01-02 03:04:06;1,37625;250\n",
    );
    assert_eq!(d.format.delimiter, ';');
    assert!(d.format.decimal_comma);
    let t = trade(&d, 1);
    assert_eq!((t.price, t.size), (1.37625, 250.0));
    assert_eq!(d.price_decimals, 5);
}

#[test]
fn overrides_win() {
    // Two close columns read as quotes, but they're really open/close trades... say so.
    let sample = "time,a,b\n2014-01-02 03:04:05,100.0,100.5\n2014-01-02 03:04:06,100.1,100.6\n";
    assert_eq!(found(sample).format.kind, Kind::Quotes);
    let overrides = Overrides {
        kind: Some(Kind::Trades),
        columns: vec![(Role::Price, "b".into())],
        ..Overrides::default()
    };
    let d = detect(sample, &overrides).unwrap();
    assert_eq!(d.format.kind, Kind::Trades);
    assert_eq!(trade(&d, 0).price, 100.5);
    assert!(
        detect(
            sample,
            &Overrides {
                columns: vec![(Role::Price, "9".into())],
                ..Overrides::default()
            }
        )
        .is_err()
    );
    assert_eq!("bid-size".parse::<Role>().unwrap(), Role::BidSize);
}

#[test]
fn says_what_it_cannot_work_out() {
    let error = detect("hello world\nfoo bar\n", &Overrides::default()).unwrap_err();
    assert!(format!("{error:#}").contains("--delimiter"), "{error:#}");
    let error = detect("a,b\nx,y\nz,w\n", &Overrides::default()).unwrap_err();
    assert!(format!("{error:#}").contains("--time"), "{error:#}");
}

#[test]
fn describes_what_it_found() {
    let d = found(
        "Gmt time,Ask,Bid\n02.01.2014 03:04:05.000,1.37698,1.37624\n13.01.2014 03:04:05.000,1.37698,1.37624\n",
    );
    let text = d.describe();
    assert!(
        text.starts_with("comma-separated, with a header row, quotes"),
        "{text}"
    );
    assert!(text.contains("bid       column 3   \"Bid\""), "{text}");
    assert!(text.contains("day first, UTC"), "{text}");
}

#[test]
fn metatrader_bar_exports() {
    // Date and time apart, no header: open, high, low, close, tick volume.
    let d = found(
        "2014.01.02,03:00,1.37650,1.37702,1.37598,1.37624,1520
         2014.01.02,04:00,1.37624,1.37730,1.37610,1.37700,1310
         2014.01.02,05:00,1.37700,1.37712,1.37500,1.37530,990
",
    );
    assert_eq!(d.format.kind, Kind::Bars);
    assert_eq!(d.spacing, Some(Spacing::Minutes(60)));
    assert_eq!(d.what(), "1h bars");
    let b = bar(&d, 0);
    assert_eq!(
        (b.ts, b.open, b.high, b.low, b.close, b.volume),
        (
            T - 245 * 1_000_000_000,
            1.3765,
            1.37702,
            1.37598,
            1.37624,
            1520.0
        )
    );
    assert_eq!(d.price_decimals, 5);
    assert!(
        d.describe().contains("volume    column 7"),
        "{}",
        d.describe()
    );
}

#[test]
fn yahoo_daily_bars_with_a_header() {
    let d = found(
        "Date,Open,High,Low,Close,Adj Close,Volume
         2024-01-02,187.15,188.44,183.89,185.64,184.94,82488700
         2024-01-03,184.22,185.88,183.43,184.25,183.55,58414500
         2024-01-04,182.15,183.09,180.88,181.91,181.22,71983600
         2024-01-05,181.99,182.76,180.17,181.18,180.49,62303300
         2024-01-08,182.09,185.60,181.50,185.56,184.86,59144500
",
    );
    assert_eq!(d.format.kind, Kind::Bars);
    let c = d.format.columns;
    assert_eq!(
        (c.open, c.high, c.low, c.close, c.size),
        (Some(1), Some(2), Some(3), Some(4), Some(6))
    );
    assert_eq!(
        d.spacing,
        Some(Spacing::Minutes(1_440)),
        "weekends don't make it weekly"
    );
    assert_eq!(bar(&d, 2).close, 181.91);
}

#[test]
fn binance_klines_without_a_header() {
    // open time, open, high, low, close, volume, close time, quote volume, trades, taker buys…
    let d = found(
        "1388631600000,42000.1,42100.0,41950.5,42050.0,12.5,1388631659999,525000.1,310,6.1,256000.2,0
         1388631660000,42050.0,42080.0,42010.0,42070.0,8.25,1388631719999,347000.4,201,4.0,168000.9,0
         1388631720000,42070.0,42070.0,41900.0,41920.0,15.0,1388631779999,628000.0,402,7.2,301000.3,0
",
    );
    assert_eq!(d.format.kind, Kind::Bars);
    assert_eq!(d.format.columns.time, 0);
    assert_eq!(d.format.columns.size, Some(5));
    assert_eq!(d.spacing, Some(Spacing::Minutes(1)));
    assert_eq!(bar(&d, 1).ts, 1_388_631_660 * 1_000_000_000);
}

#[test]
fn weekly_and_monthly_bars() {
    let weekly = found(
        "time,open,high,low,close
2024-01-01,10,12,9,11
2024-01-08,11,13,10,12
2024-01-15,12,12.5,11,11.5
",
    );
    assert_eq!(weekly.spacing, Some(Spacing::Week));
    assert_eq!(weekly.what(), "1w bars without volume");
    let monthly = found(
        "time,open,high,low,close,volume
2024-01-01,10,12,9,11,5
2024-02-01,11,13,10,12,6
2024-03-01,12,12.5,11,11.5,7
",
    );
    assert_eq!(monthly.spacing, Some(Spacing::Month));
    // Only a close: a series of prices.
    let closes = found(
        "time,close
2024-01-01 00:00,10
2024-01-01 00:01,11
",
    );
    assert_eq!(closes.format.kind, Kind::Trades);
    assert_eq!(closes.format.columns.price, Some(1));
}

// --- Importing -------------------------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("persephone-import-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two days of trades, a trade every 10 seconds, with one broken row.
fn trades_csv() -> String {
    let mut csv = String::from("time,price,qty,side\n");
    for i in 0..(2 * 8_640) {
        let ts = T / MS + i * 10_000;
        let side = if i % 2 == 0 { "buy" } else { "sell" };
        csv.push_str(&format!(
            "{ts},{:.2},0.5,{side}\n",
            100.0 + (i % 100) as f64 / 10.0
        ));
        if i == 50 {
            csv.push_str("1388632345000,oops,0.5,buy\n");
        }
    }
    csv
}

fn options() -> MarketOptions {
    MarketOptions {
        symbol: "BTCUSD".into(),
        venue: DEFAULT_VENUE.into(),
        base: None,
        quote: None,
        fee: None,
    }
}

#[test]
fn imports_plain_gzipped_and_zipped_files_alike() {
    let dir = temp_dir("packing");
    let text = trades_csv();
    let plain = dir.join("trades.csv");
    fs::write(&plain, &text).unwrap();
    let gz = dir.join("trades.csv.gz");
    let mut encoder =
        flate2::write::GzEncoder::new(fs::File::create(&gz).unwrap(), flate2::Compression::fast());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap();
    let zipped = dir.join("trades.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&zipped).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("trades.csv", stored).unwrap();
    zip.write_all(text.as_bytes()).unwrap();
    zip.finish().unwrap();

    for (name, file) in [("plain", &plain), ("gz", &gz), ("zip", &zipped)] {
        let sample = sample(file).unwrap();
        let format = found(&sample).format;
        let store = Store::new(dir.join(format!("store-{name}")));
        let summary = import(
            &store,
            std::slice::from_ref(file),
            &format,
            &options(),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(summary.imported, 2 * 8_640, "{name}");
        assert_eq!(summary.skipped, 1, "{name}");
        assert!(
            summary.problems[0].contains("line 53") && summary.problems[0].contains("oops"),
            "{:?}",
            summary.problems
        );

        let market = summary.market;
        assert_eq!(
            (market.info.base.as_str(), market.info.quote.as_str()),
            ("BTC", "USD")
        );
        assert_eq!(
            (market.info.price_precision, market.info.size_precision),
            (1, 1)
        );
        assert_eq!(market.info.fee, 0.001);
        assert_eq!(market.info.count, 2 * 8_640);
        assert_eq!(
            market.days().len(),
            3,
            "2 days from 03:04 spill into a third"
        );
        let Ticks::Trades(trades) = market.read_ticks(0, u64::MAX).unwrap() else {
            unreachable!()
        };
        assert_eq!(trades[1].aggressor, Aggressor::Seller);
        let hours = market.candles(Timeframe::H1, 1_000).unwrap();
        assert_eq!(
            hours.iter().map(|b| b.volume).sum::<f64>(),
            8_640.0,
            "all the volume"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reimporting_the_same_file_changes_nothing() {
    let dir = temp_dir("again");
    let file = dir.join("trades.csv");
    fs::write(&file, trades_csv()).unwrap();
    let format = found(&sample(&file).unwrap()).format;
    let store = Store::new(dir.join("store"));
    let once = import(
        &store,
        std::slice::from_ref(&file),
        &format,
        &options(),
        &mut |_| true,
    )
    .unwrap();
    let twice = import(&store, &[file], &format, &options(), &mut |_| true).unwrap();
    assert_eq!(twice.market.info.count, once.market.info.count);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unsorted_files_and_wrong_kinds() {
    let dir = temp_dir("unsorted");
    let file = dir.join("ticks.csv");
    // Days out of order: the second day first.
    let day = 86_400_000;
    let rows: Vec<String> = [1, 0, 1, 0]
        .iter()
        .enumerate()
        .map(|(i, d)| format!("{},1.{i},1", T / MS + d * day + i as u64))
        .collect();
    fs::write(&file, rows.join("\n")).unwrap();
    let format = found(&sample(&file).unwrap()).format;
    let store = Store::new(dir.join("store"));
    let summary = import(
        &store,
        std::slice::from_ref(&file),
        &format,
        &options(),
        &mut |_| true,
    )
    .unwrap();
    assert_eq!(summary.market.info.count, 4);

    let quotes = Format {
        kind: Kind::Quotes,
        ..format
    };
    let error = import(&store, &[file], &quotes, &options(), &mut |_| true).unwrap_err();
    assert!(error.to_string().contains("holds trades"), "{error}");
    fs::remove_dir_all(dir).unwrap();
}

/// Hourly bars from 2023-12-31 22:00 UTC for `hours`, written in New York time.
fn hourly_csv(hours: u64) -> String {
    let start = persephone_core::time::days_from_civil(2023, 12, 31) as UnixNanos * 86_400
        + 22 * 3_600
        - 5 * 3_600;
    let mut csv = String::from(
        "Date,Time,Open,High,Low,Close,Volume
",
    );
    for h in 0..hours {
        let dt = persephone_core::time::DateTime::from_nanos((start + h * 3_600) * 1_000_000_000);
        let price = 100.0 + (h % 24) as f64;
        csv.push_str(&format!(
            "{},{},{price},{},{},{},10
",
            dt.date(),
            dt.time(),
            price + 2.0,
            price - 1.0,
            price + 1.0
        ));
    }
    csv
}

#[test]
fn imports_bars_and_charts_them_at_their_spacing_and_up() {
    let dir = temp_dir("bars");
    let file = dir.join("EURUSD_H1.csv");
    fs::write(&file, hourly_csv(24 * 40)).unwrap();
    let detection = detect(
        &sample(&file).unwrap(),
        &Overrides {
            zone: Some("America/New_York".parse().unwrap()),
            ..Overrides::default()
        },
    )
    .unwrap();
    assert_eq!(detection.spacing, Some(Spacing::Minutes(60)));
    let store = Store::new(dir.join("store"));
    let import_it = |file: &PathBuf| {
        import(
            &store,
            std::slice::from_ref(file),
            &detection.format,
            &options(),
            &mut |_| true,
        )
    };
    let summary = import_it(&file).unwrap();
    assert_eq!(summary.imported, 960);
    let market = summary.market;
    assert_eq!(market.info.kind, MarketKind::Bars(Spacing::Minutes(60)));
    assert_eq!(market.info.count, 960);
    assert_eq!(market.info.fee, 0.001);
    assert_eq!(
        market.info.first,
        persephone_core::time::days_from_civil(2023, 12, 31) as UnixNanos * NANOS_PER_DAY
            + 22 * 3_600_000_000_000,
        "read in New York time"
    );
    assert!(market.dir().join("bars/2023.parquet").exists());
    assert!(market.dir().join("bars/2024.parquet").exists());
    assert_eq!(
        import_it(&file).unwrap().market.info.count,
        960,
        "again replaces"
    );

    assert_eq!(
        market.candles(Timeframe::H4, 1_000).unwrap().len(),
        241,
        "from 22:00, the first 4 hours hold two"
    );
    let daily = market.candles(Timeframe::D1, 1_000).unwrap();
    assert_eq!(daily.len(), 41, "40 days from 22:00 touch 41");
    assert_eq!(daily[1].volume, 240.0);
    assert!(market.candles(Timeframe::M15, 10).is_err());

    // Daily bars can't join an hourly market.
    let days = dir.join("EURUSD_D1.csv");
    fs::write(
        &days,
        "Date,Open,High,Low,Close
2024-03-01,1,2,0.5,1.5
2024-03-04,1.5,2,1,1.2
2024-03-05,1.2,1.3,1,1.1
",
    )
    .unwrap();
    let daily_format = found(&sample(&days).unwrap()).format;
    let error = import(&store, &[days], &daily_format, &options(), &mut |_| true).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("holds 1h bars, but these files have 1d bars"),
        "{error}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn profiles_round_trip() {
    let dir = temp_dir("profiles");
    let format = found("Gmt time,Ask,Bid\n02.01.2014 03:04:05.000,1.37698,1.37624\n").format;
    let format = Format {
        zone: "America/New_York".parse().unwrap(),
        ..format
    };
    save_profile(&dir, "dukascopy", &format).unwrap();
    assert_eq!(load_profile(&dir, "dukascopy").unwrap(), format);
    assert!(save_profile(&dir, "../evil", &format).is_err());
    assert!(load_profile(&dir, "missing").is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn symbols_split_into_currencies() {
    let pair = |s: &str| split_symbol(s).map(|(b, q)| format!("{b}/{q}"));
    assert_eq!(pair("EURUSD").as_deref(), Some("EUR/USD"));
    assert_eq!(pair("BTCUSDT").as_deref(), Some("BTC/USDT"));
    assert_eq!(pair("XBTEUR").as_deref(), Some("XBT/EUR"));
    assert_eq!(pair("DOGEUSDT").as_deref(), Some("DOGE/USDT"));
    assert_eq!(pair("SPX500"), None);
}

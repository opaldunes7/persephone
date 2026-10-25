//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`, e.g. `persephone ETHUSDT 5m`.
//! Defaults to Binance BTCUSDT 1m.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use iced::{
    Element, Fill, Task, Theme,
    futures::channel::oneshot,
    widget::{container, text},
};
use persephone_chart::{ChartData, ChartMessage, ChartState, theme};
use persephone_core::Timeframe;
use persephone_data::binance::BinanceClient;

/// How many bars to load at startup.
const HISTORY_BARS: u64 = 5_000;

fn main() -> iced::Result {
    iced::application(Persephone::boot, Persephone::update, Persephone::view)
        .title(Persephone::title)
        .theme(Theme::Dark)
        .window_size((1280.0, 800.0))
        .run()
}

enum Persephone {
    Loading {
        symbol: String,
        timeframe: Timeframe,
    },
    Ready(ChartState),
    Failed(String),
}

#[derive(Debug, Clone)]
enum Message {
    Loaded(Result<Arc<ChartData>, String>),
    Chart(ChartMessage),
}

impl Persephone {
    fn boot() -> (Self, Task<Message>) {
        let mut args = std::env::args().skip(1);
        let symbol = args
            .next()
            .unwrap_or_else(|| "BTCUSDT".into())
            .to_uppercase();
        let timeframe = match args.next().map(|tf| tf.parse::<Timeframe>()) {
            Some(Ok(tf)) => tf,
            Some(Err(e)) => return (Self::Failed(e.to_string()), Task::none()),
            None => Timeframe::M1,
        };
        let task = Task::perform(load(symbol.clone(), timeframe), Message::Loaded);
        (Self::Loading { symbol, timeframe }, task)
    }

    fn title(&self) -> String {
        match self {
            Self::Ready(chart) => {
                let data = chart.data();
                format!("{} · {} — Persephone", data.symbol, data.timeframe)
            }
            _ => "Persephone".into(),
        }
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Loaded(Ok(data)) => *self = Self::Ready(ChartState::new(data)),
            Message::Loaded(Err(error)) => *self = Self::Failed(error),
            Message::Chart(message) => {
                if let Self::Ready(chart) = self {
                    chart.update(message);
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let status = match self {
            Self::Ready(chart) => return chart.view().map(Message::Chart),
            Self::Loading { symbol, timeframe } => {
                text(format!("Loading {symbol} {timeframe} from Binance…")).color(theme::TEXT)
            }
            Self::Failed(error) => text(error).color(theme::DOWN),
        };
        container(status.size(14))
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| container::background(theme::BACKGROUND))
            .into()
    }
}

/// Fetches bars on a plain thread (the Binance client is blocking) and hands them back to
/// iced's executor.
async fn load(symbol: String, timeframe: Timeframe) -> Result<Arc<ChartData>, String> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch(&symbol, timeframe).map_err(|e| format!("{e:#}")));
    });
    rx.await
        .map_err(|_| "the loader thread stopped".to_string())?
}

fn fetch(symbol: &str, timeframe: Timeframe) -> anyhow::Result<Arc<ChartData>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % timeframe.nanos() + timeframe.nanos(); // include the forming bar
    let start = end.saturating_sub(HISTORY_BARS * timeframe.nanos());

    let binance = BinanceClient::new()?;
    let spec = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, start, end)?;
    anyhow::ensure!(!candles.is_empty(), "Binance returned no bars for {symbol}");
    Ok(ChartData::new(
        spec.symbol,
        spec.venue,
        timeframe,
        spec.price_precision,
        candles,
    ))
}

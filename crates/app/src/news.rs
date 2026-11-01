//! The news sidebar: headlines from public feeds down the window's right edge, opened from a
//! chart's right-click menu or Ctrl+N. Live, it shows the latest; while a backtest plays back,
//! only what had been published by the playhead, so it never gives away what came next.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iced::futures::{SinkExt, Stream, StreamExt, channel::mpsc};
use persephone_core::UnixNanos;
use persephone_data::news::{
    FeedClient, GDELT_PAUSE, Headline, HistorySearch, NewsStore, feeds, market_query,
};
use serde::{Deserialize, Serialize};

use crate::Message;

pub mod view;

/// How often the feeds are read.
const EVERY: Duration = Duration::from_secs(5 * 60);
/// The sidebar's narrowest and widest, and what it opens at.
pub const MIN_WIDTH: f32 = 220.0;
pub const MAX_WIDTH: f32 = 520.0;
pub const DEFAULT_WIDTH: f32 = 300.0;
/// What the charts keep, however wide the sidebar's dragged.
const CHARTS_ROOM: f32 = 400.0;

/// Which headlines the sidebar lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    #[default]
    All,
    /// Those naming the focused chart's market.
    Market,
    /// The economy and regulators.
    Macro,
}

/// The sidebar as it's saved with the layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewsSaved {
    pub open: bool,
    pub width: f32,
    /// Read the feeds even while it's closed, so the history has no gaps. Off until the sidebar's
    /// first opened, so nothing's fetched for someone who never uses it.
    pub collecting: bool,
    pub filter: Filter,
    /// It's been opened before: collecting has been turned on once, and is left as set.
    pub used: bool,
}

impl Default for NewsSaved {
    fn default() -> Self {
        Self {
            open: false,
            width: DEFAULT_WIDTH,
            collecting: false,
            filter: Filter::All,
            used: false,
        }
    }
}

/// What the sidebar asks for.
#[derive(Clone, Debug)]
pub enum NewsMessage {
    Toggle,
    Filter(Filter),
    Collecting(bool),
    /// Read the feeds now.
    Refresh,
    /// The feeds' headlines, and what couldn't be read.
    Fetched(Arc<Vec<Headline>>, Vec<String>),
    /// The sidebar's left edge dragged to this x.
    Width(f32),
    /// Go to this headline's time on the chart, by its id.
    Pick(String),
    /// Light this headline in the list and scroll to it, as its mark on a chart was clicked.
    Show(String),
    /// Open this link in the browser.
    Open(String),
    /// Search GDELT's archive for older headlines over the focused chart's bars.
    FetchHistory,
    StopHistory,
    /// One search's headlines, the search's key and when its stretch ends, and how many are done.
    HistoryFound(Arc<Vec<Headline>>, String, UnixNanos, usize),
    HistoryFailed(String),
    HistoryEnded,
}

/// Searching GDELT's archive for older headlines.
#[derive(Debug, Default)]
pub enum History {
    #[default]
    Idle,
    Running {
        done: usize,
        total: usize,
        added: usize,
        stop: Arc<AtomicBool>,
    },
    /// How many headlines it added, and whether there was nothing left to search.
    Finished {
        added: usize,
        nothing_left: bool,
    },
    Failed(String),
}

/// The sidebar's state, and every headline collected.
#[derive(Debug, Default)]
pub struct News {
    pub saved: NewsSaved,
    /// Where headlines are kept; `None` keeps them only in memory, as in tests.
    pub root: Option<PathBuf>,
    pub store: NewsStore,
    /// When the feeds were last read, and those that couldn't be.
    pub fetched_at: Option<UnixNanos>,
    pub failures: Vec<String>,
    /// The headline last picked or clicked on a chart, lit in the list.
    pub lit: Option<String>,
    pub history: History,
    /// Said under the filters, about the last headline picked.
    pub note: Option<String>,
}

impl News {
    #[must_use]
    pub fn from_saved(saved: Option<NewsSaved>) -> Self {
        Self {
            saved: saved.unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Reads the headlines kept under `root`, and keeps new ones there from now on.
    pub fn use_root(&mut self, root: PathBuf) {
        self.store = NewsStore::open(&root);
        self.root = Some(root);
    }

    /// How wide the sidebar is in a window `window` wide; 0 while it's closed.
    #[must_use]
    pub fn width(&self, window: f32) -> f32 {
        if !self.saved.open {
            return 0.0;
        }
        self.saved
            .width
            .clamp(MIN_WIDTH, MAX_WIDTH)
            .min((window - CHARTS_ROOM).max(0.0))
    }

    /// Whether the feeds should be read now.
    #[must_use]
    pub fn polling(&self) -> bool {
        self.root.is_some() && (self.saved.open || self.saved.collecting)
    }

    /// Adds what the feeds had, keeping it on disk.
    pub fn fetched(&mut self, found: &[Headline], failures: Vec<String>, now: UnixNanos) {
        if let Err(error) = self.store.add(found.to_vec()) {
            eprintln!("persephone: couldn't keep the headlines: {error}");
        }
        self.fetched_at = Some(now);
        self.failures = failures;
    }

    /// The headline with this id.
    #[must_use]
    pub fn headline(&self, id: &str) -> Option<&Headline> {
        self.store.all().iter().find(|headline| headline.id == id)
    }
}

/// Reads every feed now, off the app's thread.
pub fn fetch_now(root: PathBuf) -> iced::Task<Message> {
    iced::Task::perform(
        async move {
            std::thread::spawn(move || read_feeds(&root))
                .join()
                .unwrap_or_else(|_| (Vec::new(), vec!["The feed reader stopped.".into()]))
        },
        |(found, failed)| Message::News(NewsMessage::Fetched(Arc::new(found), failed)),
    )
}

fn read_feeds(root: &std::path::Path) -> (Vec<Headline>, Vec<String>) {
    match FeedClient::new() {
        Ok(client) => client.fetch_all(&feeds(root)),
        Err(error) => (Vec::new(), vec![error]),
    }
}

/// Makes `searches` of GDELT's archive one at a time, as slowly as it asks, until done or stopped.
fn search_history(searches: Vec<HistorySearch>, stop: Arc<AtomicBool>) -> iced::Task<Message> {
    let stream = iced::stream::channel(4, async move |mut output: mpsc::Sender<Message>| {
        let (tx, mut rx) = mpsc::unbounded();
        std::thread::spawn(move || {
            let client = match FeedClient::new() {
                Ok(client) => client,
                Err(error) => {
                    let _ = tx.unbounded_send(Err(error));
                    return;
                }
            };
            for (at, search) in searches.into_iter().enumerate() {
                if at > 0 {
                    std::thread::sleep(GDELT_PAUSE);
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let result = client
                    .history(&search.query, search.from, search.to)
                    .map(|found| (found, search, at + 1));
                let failed = result.is_err();
                if tx.unbounded_send(result).is_err() || failed {
                    break;
                }
            }
        });
        while let Some(result) = rx.next().await {
            let message = match result {
                Ok((found, search, done)) => {
                    NewsMessage::HistoryFound(Arc::new(found), search.key, search.to, done)
                }
                Err(error) => NewsMessage::HistoryFailed(error),
            };
            if output.send(Message::News(message)).await.is_err() {
                return;
            }
        }
        let _ = output.send(Message::News(NewsMessage::HistoryEnded)).await;
    });
    iced::Task::run(stream, |message| message)
}

/// A day, for a stretch that's only just ended and may still get more.
const DAY: UnixNanos = 86_400_000_000_000;

/// Reads the feeds every few minutes, as long as it's subscribed to.
pub fn poll(root: &std::path::Path) -> impl Stream<Item = Message> + use<> {
    let root = root.to_path_buf();
    iced::stream::channel(4, async move |mut output: mpsc::Sender<Message>| {
        let (tx, mut rx) = mpsc::unbounded();
        // The network is slow and blocking, so it's read on a thread of its own, which stops once
        // nothing's listening.
        std::thread::spawn(move || {
            loop {
                let (found, failed) = read_feeds(&root);
                if tx.unbounded_send((found, failed)).is_err() {
                    break;
                }
                std::thread::sleep(EVERY);
                if tx.is_closed() {
                    break;
                }
            }
        });
        while let Some((found, failed)) = rx.next().await {
            let message = Message::News(NewsMessage::Fetched(Arc::new(found), failed));
            if output.send(message).await.is_err() {
                break;
            }
        }
    })
}

fn now() -> UnixNanos {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as UnixNanos)
}

impl crate::Persephone {
    /// What the focused chart's market is called in headlines, and the name to put on its chip.
    fn market_words(&self) -> (Vec<String>, String) {
        let symbol = self
            .tiles
            .get(&self.layout.focused())
            .map_or("BTCUSDT", |tile| tile.market.symbol.as_str());
        let words = persephone_data::news::market_words(symbol);
        let chip = words.first().cloned().unwrap_or_else(|| "Market".into());
        (words, chip)
    }

    /// The playhead's time, while a backtest is played back.
    fn news_replay_time(&self) -> Option<UnixNanos> {
        self.panel.as_ref().and_then(|panel| panel.playback.at)
    }

    pub(crate) fn news_view(&self) -> iced::Element<'_, Message> {
        let (words, chip) = self.market_words();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        // What the focused chart covers, so headlines off it can say so.
        let span = self
            .tiles
            .get(&self.layout.focused())
            .and_then(|tile| tile.chart.as_ref())
            .and_then(|chart| {
                let data = chart.data();
                let first = data.candles.first()?.ts;
                let last = data.candles.last()?.ts;
                Some((first, last.saturating_add(data.typical_nanos.max(1))))
            });
        view::sidebar(
            &self.news,
            self.news_replay_time(),
            &words,
            &chip,
            now(),
            span,
        )
    }

    pub(crate) fn on_news(&mut self, message: NewsMessage) -> iced::Task<Message> {
        match message {
            NewsMessage::Toggle => {
                self.news.saved.open = !self.news.saved.open;
                // Its first use starts collecting, which stays as set from then on.
                if self.news.saved.open && !self.news.saved.used {
                    self.news.saved.used = true;
                    self.news.saved.collecting = true;
                }
                self.relayout(true);
                self.save();
                self.sync_news_marks();
                // Opened for the first time this session: read the feeds now, not in 5 minutes.
                if self.news.saved.open
                    && self.news.fetched_at.is_none()
                    && let Some(root) = self.news.root.clone()
                {
                    return fetch_now(root);
                }
            }
            NewsMessage::Filter(filter) => {
                self.news.saved.filter = filter;
                self.save();
                self.sync_news_marks();
            }
            NewsMessage::Collecting(on) => {
                self.news.saved.collecting = on;
                self.save();
            }
            NewsMessage::Refresh => {
                if let Some(root) = self.news.root.clone() {
                    return fetch_now(root);
                }
            }
            NewsMessage::Fetched(found, failures) => {
                self.news.fetched(&found, failures, now());
                self.sync_news_marks();
            }
            NewsMessage::Show(id) => {
                self.light_news(Some(id.clone()));
                let (words, _) = self.market_words();
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                let listed = view::shown(&self.news, self.news_replay_time(), &words);
                let place = listed.iter().position(|headline| headline.id == id);
                let count = listed.len();
                self.news.lit = Some(id);
                if let Some(place) = place {
                    let share = place as f32 / count.saturating_sub(1).max(1) as f32;
                    return iced::widget::operation::snap_to(
                        view::list_id(),
                        iced::widget::operation::RelativeOffset {
                            x: None,
                            y: Some(share),
                        },
                    );
                }
            }
            NewsMessage::Width(x) => {
                self.news.saved.width = (self.window.width - x).clamp(MIN_WIDTH, MAX_WIDTH);
                self.relayout(false);
                self.save();
            }
            NewsMessage::Pick(id) => {
                let Some(at) = self.news.headline(&id).map(|h| h.published) else {
                    return iced::Task::none();
                };
                self.news.lit = Some(id.clone());
                self.news.note = None;
                // Its mark shows whatever the filter, and stands out from the rest.
                self.light_news(Some(id));
                // Replaying: the playhead goes there too, as far as the span allows.
                let mut task = iced::Task::none();
                if self.news_replay_time().is_some() {
                    task = self.on_panel(crate::backtest::PanelMessage::Seek(at));
                }
                // Every chart on screen goes there, so whichever is being looked at shows it.
                let focused = self.layout.focused();
                for tile in self.layout.tiles() {
                    let width = self.tile_rect(tile).map_or(800.0, |rect| {
                        (rect.width - persephone_chart::PRICE_AXIS_WIDTH).max(100.0)
                    });
                    let Some(chart) = self.tiles.get_mut(&tile).and_then(|t| t.chart.as_mut())
                    else {
                        continue;
                    };
                    let ends = chart
                        .data()
                        .candles
                        .first()
                        .zip(chart.data().candles.last())
                        .map(|(first, last)| (first.ts, last.ts));
                    if !chart.center_on_time(at, width) && tile == focused {
                        let date = |ts| persephone_core::time::DateTime::from_nanos(ts).date();
                        self.news.note = Some(match ends {
                            Some((first, _)) if at < first => format!(
                                "That's from before this chart's first bar, {}. Widen its span to see it.",
                                date(first)
                            ),
                            Some((_, last)) => format!(
                                "That's from after this chart's last bar, {}. Load later bars to see it.",
                                date(last)
                            ),
                            None => "This chart has no bars yet.".into(),
                        });
                    }
                }
                return task;
            }
            NewsMessage::FetchHistory => return self.fetch_history(),
            NewsMessage::StopHistory => {
                if let History::Running { stop, added, .. } = &self.news.history {
                    stop.store(true, Ordering::Relaxed);
                    self.news.history = History::Finished {
                        added: *added,
                        nothing_left: false,
                    };
                }
            }
            NewsMessage::HistoryFound(found, key, to, done_now) => {
                let added_now = match self.news.store.add(found.to_vec()) {
                    Ok(added) => added.len(),
                    Err(error) => {
                        eprintln!("persephone: couldn't keep the headlines: {error}");
                        0
                    }
                };
                // A stretch still going may get more, so it's searched again next time.
                if to.saturating_add(DAY) < now()
                    && let Err(error) = self.news.store.mark_searched(key)
                {
                    eprintln!("persephone: couldn't note the search: {error}");
                }
                if let History::Running { done, added, .. } = &mut self.news.history {
                    *done = done_now;
                    *added += added_now;
                }
                self.sync_news_marks();
            }
            NewsMessage::HistoryFailed(error) => {
                self.news.history = History::Failed(error);
            }
            NewsMessage::HistoryEnded => {
                if let History::Running { added, .. } = &self.news.history {
                    self.news.history = History::Finished {
                        added: *added,
                        nothing_left: false,
                    };
                }
            }
            NewsMessage::Open(link) => {
                if !link.is_empty() {
                    std::thread::spawn(move || {
                        if let Err(error) = opener::open_browser(&link) {
                            eprintln!("persephone: couldn't open {link}: {error}");
                        }
                    });
                }
            }
        }
        iced::Task::none()
    }

    /// The marks a chart of `symbol` shows: none while the sidebar's closed, else the headlines
    /// that matter to it, by the sidebar's filter.
    fn news_marks_for(&self, symbol: &str) -> Vec<persephone_chart::EventMark> {
        if !self.news.saved.open {
            return Vec::new();
        }
        let words = persephone_data::news::market_words(symbol);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        self.news
            .store
            .all()
            .iter()
            .filter(|headline| {
                if self.news.lit.as_deref() == Some(headline.id.as_str()) {
                    return true;
                }
                let high = headline.importance(&words) == persephone_data::news::Importance::High;
                match self.news.saved.filter {
                    Filter::All => high || headline.names_market(&words),
                    Filter::Market => headline.names_market(&words),
                    Filter::Macro => headline.is_macro(),
                }
            })
            .map(|headline| persephone_chart::EventMark {
                ts: headline.published,
                high: headline.importance(&words) == persephone_data::news::Importance::High,
                title: headline.title.clone(),
                id: headline.id.clone(),
            })
            .collect()
    }

    /// Lights the headline `id` on every chart, its mark shown whatever the filter.
    fn light_news(&mut self, id: Option<String>) {
        self.news.lit.clone_from(&id);
        self.sync_news_marks();
        for tile in self.tiles.values_mut() {
            if let Some(chart) = tile.chart.as_mut() {
                chart.light_event(id.clone());
            }
        }
    }

    /// Searches GDELT's archive for the headlines over the focused chart's bars that haven't been
    /// searched for yet: its market's and the economy's.
    fn fetch_history(&mut self) -> iced::Task<Message> {
        if matches!(self.news.history, History::Running { .. }) {
            return iced::Task::none();
        }
        let Some((first, last, symbol)) = self.tiles.get(&self.layout.focused()).and_then(|tile| {
            let candles = &tile.chart.as_ref()?.data().candles;
            Some((
                candles.first()?.ts,
                candles.last()?.ts,
                tile.market.symbol.clone(),
            ))
        }) else {
            self.news.history = History::Failed("Open a chart with bars first.".into());
            return iced::Task::none();
        };
        let words = persephone_data::news::market_words(&symbol);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let query = market_query(&words);
        // Newest first: what's nearest now is most often looked at.
        let mut searches =
            self.news
                .store
                .searches_left(query.as_deref(), first, last.saturating_add(DAY));
        searches.reverse();
        if searches.is_empty() {
            self.news.history = History::Finished {
                added: 0,
                nothing_left: true,
            };
            return iced::Task::none();
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.news.history = History::Running {
            done: 0,
            total: searches.len(),
            added: 0,
            stop: Arc::clone(&stop),
        };
        search_history(searches, stop)
    }

    /// Puts each chart's headline marks on it, as the sidebar and its filter have them.
    pub(crate) fn sync_news_marks(&mut self) {
        let ids: Vec<crate::TileId> = self.tiles.keys().copied().collect();
        for id in ids {
            self.sync_news_marks_on(id);
        }
    }

    /// Puts tile `id`'s headline marks on its chart.
    pub(crate) fn sync_news_marks_on(&mut self, id: crate::TileId) {
        let Some(symbol) = self.tiles.get(&id).map(|tile| tile.market.symbol.clone()) else {
            return;
        };
        let marks = self.news_marks_for(&symbol);
        if let Some(chart) = self.tiles.get_mut(&id).and_then(|tile| tile.chart.as_mut()) {
            chart.set_events(marks);
        }
    }

    /// The news sidebar's state. For tests.
    #[doc(hidden)]
    #[must_use]
    pub fn news(&self) -> &News {
        &self.news
    }

    /// Keeps headlines under `root` rather than the data folder. For tests.
    #[doc(hidden)]
    pub fn use_news_root(&mut self, root: PathBuf) {
        self.news.use_root(root);
    }
}

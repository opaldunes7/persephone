//! An event study: the same strategy on the windows after news, and on as many windows with no
//! such news, to see whether the news is what makes the difference. One study is one test, however
//! many windows it has, as the windows are its sample rather than variants tried.

use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicBool},
};

use persephone_core::{Candle, UnixNanos};
use persephone_data::news::{Headline, Importance};
use persephone_engine::{BacktestRequest, Direction};
use rand::{SeedableRng as _, rngs::StdRng, seq::SliceRandom as _};

use crate::{
    figures,
    robustness::{BlockJob, RoundTrip},
};

const HOUR: UnixNanos = 3_600_000_000_000;

/// How many times the windows' labels are shuffled for the p-value.
pub const SHUFFLES: usize = 10_000;

/// Fewer windows than this in either group, and the comparison says little.
pub const FEWEST: usize = 10;

/// How the windows are found from news.
#[derive(Clone, Debug, PartialEq)]
pub struct NewsChoice {
    /// The least a headline must be to make an event: medium names the market; high is
    /// market-wide (rates, regulators, hacks), whatever the market.
    pub importance: Importance,
    /// Only headlines that name the market.
    pub market_only: bool,
    /// Headlines within this many hours of an event's first are part of it.
    pub merge_hours: u32,
    /// Each window starts this many hours after its event.
    pub after_hours: u32,
    /// And lasts this many hours.
    pub length_hours: u32,
    /// How many control windows; none means as many as there are events.
    pub controls: Option<usize>,
    /// The least a headline must be to keep a control window out: none means `importance`. It
    /// may be stricter (medium when events are high), not looser.
    pub control_importance: Option<Importance>,
    pub seed: u64,
}

impl Default for NewsChoice {
    fn default() -> Self {
        Self {
            importance: Importance::High,
            market_only: false,
            merge_hours: 24,
            after_hours: 4,
            length_hours: 7 * 24,
            controls: None,
            control_importance: None,
            seed: 0,
        }
    }
}

impl NewsChoice {
    /// What the app's News windows test is set to, with `seed` for its controls.
    #[must_use]
    pub fn of(settings: &crate::settings::NewsWindows, seed: u64) -> Self {
        Self {
            importance: if settings.high {
                Importance::High
            } else {
                Importance::Medium
            },
            length_hours: settings.length_days.max(1) * 24,
            after_hours: settings.after_hours,
            controls: (settings.controls > 0).then_some(settings.controls as usize),
            control_importance: (settings.high && settings.quiet_medium)
                .then_some(Importance::Medium),
            seed,
            ..Self::default()
        }
    }
}

impl FollowMove {
    /// What the app's News windows test is set to: over the hours from headline to window.
    #[must_use]
    pub fn chosen(settings: &crate::settings::NewsWindows) -> Option<Self> {
        settings.follow.then(|| Self {
            hours: settings.after_hours.max(1),
            threshold: settings.follow_percent / 100.0,
        })
    }
}

/// Which group a window is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Event,
    Control,
}

impl Kind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Control => "control",
        }
    }
}

/// One window to test, in bars of the market.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub kind: Kind,
    pub bars: Range<usize>,
    /// When its event was, and its first headline, for an event window from news.
    pub event: Option<(UnixNanos, String)>,
}

impl Window {
    /// A window of `kind` over `bars`, with no event.
    #[must_use]
    pub const fn new(kind: Kind, bars: Range<usize>) -> Self {
        Self {
            kind,
            bars,
            event: None,
        }
    }
}

/// Trade each window only the way the market moved just before it: long only after a rise
/// above `threshold`, short only after a fall below it, and either way otherwise. The move is
/// over the `hours` before the window starts, so it's known before the window trades, and
/// control windows get theirs the same way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowMove {
    pub hours: u32,
    /// As a fraction: 0.005 is 0.5%.
    pub threshold: f64,
}

impl FollowMove {
    /// The move before `bars` on `closes` at `times`, and the way it lets the window trade: none
    /// if there aren't the bars before it to measure.
    #[must_use]
    pub fn before(
        self,
        times: &[UnixNanos],
        closes: &[f64],
        bars: &Range<usize>,
    ) -> Option<(f64, Direction)> {
        let starts = *times.get(bars.start)?;
        let from =
            times.partition_point(|&t| t < starts.saturating_sub(u64::from(self.hours) * HOUR));
        let before = *closes.get(from.checked_sub(1)?)?;
        let last = *closes.get(bars.start.checked_sub(1)?)?;
        if before <= 0.0 || from >= bars.start {
            return None;
        }
        let moved = last / before - 1.0;
        let way = if moved > self.threshold {
            Direction::LongOnly
        } else if moved < -self.threshold {
            Direction::ShortOnly
        } else {
            Direction::Both
        };
        Some((moved, way))
    }
}

/// What a direction lets a window do, in a word.
#[must_use]
pub const fn way_label(way: Direction) -> &'static str {
    match way {
        Direction::Both => "both",
        Direction::LongOnly => "long",
        Direction::ShortOnly => "short",
    }
}

/// The windows found, and what to say about them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Windows {
    pub windows: Vec<Window>,
    /// How many headlines were at the importance asked, in the bars' span.
    pub headlines: usize,
    pub warnings: Vec<String>,
}

/// The bars of `[from, to)`, as indices into `times`: none if it has fewer than two.
#[must_use]
pub fn bars_of(times: &[UnixNanos], from: UnixNanos, to: UnixNanos) -> Option<Range<usize>> {
    let start = times.partition_point(|&t| t < from);
    let end = times.partition_point(|&t| t < to);
    (end >= start + 2).then_some(start..end)
}

/// The event windows and control windows for `headlines` on bars at `times`, trading from bar
/// `from_index`. `words` name the market. `gaps` are where nothing was collected, and `since` is
/// the first headline collected: controls stay where the news is known, so a quiet stretch is
/// truly quiet.
///
/// An event is a headline at the importance asked, with those within `merge_hours` of it. Its
/// window starts `after_hours` after it and lasts `length_hours`, and is dropped if it doesn't fit
/// in the bars. A control window is as long, starts on a bar picked at random with `seed`, and has
/// no such headline in it or in the `after_hours` before it. Controls overlap neither each other
/// nor an event window, and stay out of gaps.
#[must_use]
pub fn from_news(
    times: &[UnixNanos],
    from_index: usize,
    headlines: &[Headline],
    words: &[&str],
    gaps: &[(UnixNanos, UnixNanos)],
    since: Option<UnixNanos>,
    choice: &NewsChoice,
) -> Windows {
    let mut warnings = Vec::new();
    let (Some(&first), Some(&last)) = (times.get(from_index), times.last()) else {
        return Windows {
            warnings: vec!["There are no bars to trade.".into()],
            ..Windows::default()
        };
    };
    let after = u64::from(choice.after_hours) * HOUR;
    let length = u64::from(choice.length_hours.max(1)) * HOUR;
    let merge = u64::from(choice.merge_hours) * HOUR;
    // A window fits if it ends by the last bar's end, a bar's width after its time.
    let end = last
        + times
            .len()
            .checked_sub(2)
            .map_or(HOUR, |at| last - times[at]);
    // The headlines that count, oldest first, from whenever they could start a window that fits.
    let at_least = |least: Importance| {
        let mut counted: Vec<&Headline> = headlines
            .iter()
            .filter(|h| h.importance(words) >= least)
            .filter(|h| !choice.market_only || h.names_market(words))
            .filter(|h| h.published <= last)
            .collect();
        counted.sort_by_key(|h| h.published);
        counted
    };
    let counted = at_least(choice.importance);
    let quiet_of = choice
        .control_importance
        .map_or(choice.importance, |least| least.min(choice.importance));
    let in_span = counted
        .iter()
        .filter(|h| h.published + after >= first)
        .count();

    let mut events: Vec<Window> = Vec::new();
    let mut dropped = 0;
    let mut event_start: Option<UnixNanos> = None;
    for headline in &counted {
        if event_start.is_some_and(|start| headline.published <= start + merge) {
            continue;
        }
        event_start = Some(headline.published);
        let starts = headline.published + after;
        if starts < first {
            continue;
        }
        match bars_of(times, starts, starts + length).filter(|_| starts + length <= end) {
            Some(bars) => events.push(Window {
                kind: Kind::Event,
                bars,
                event: Some((headline.published, headline.title.clone())),
            }),
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        warnings.push(format!(
            "{dropped} event{} left out: {} window would run past the last bar.",
            plural(dropped),
            if dropped == 1 { "its" } else { "their" }
        ));
    }
    let overlapping = events
        .windows(2)
        .filter(|pair| pair[1].bars.start < pair[0].bars.end)
        .count();
    if overlapping > 0 {
        warnings.push(format!(
            "{overlapping} event window{} overlap the one before, so they share bars and aren't independent. A shorter length, or a longer merge, keeps them apart.",
            plural(overlapping)
        ));
    }

    let wanted = choice.controls.unwrap_or(events.len());
    let span = bars_of(times, first, first + length).map_or(0, |bars| bars.len());
    let mut starts: Vec<usize> = (from_index..times.len()).collect();
    starts.shuffle(&mut StdRng::seed_from_u64(choice.seed));
    let published: Vec<UnixNanos> = at_least(quiet_of).iter().map(|h| h.published).collect();
    let quiet = |from: UnixNanos, to: UnixNanos| {
        let at = published.partition_point(|&p| p < from);
        published.get(at).is_none_or(|&p| p >= to)
    };
    let known = |from: UnixNanos, to: UnixNanos| {
        since.is_some_and(|since| since <= from)
            && gaps
                .iter()
                .all(|&(gap_from, gap_to)| gap_to <= from || gap_from >= to)
    };
    let mut controls: Vec<Window> = Vec::new();
    if span >= 2 {
        for start in starts {
            if controls.len() >= wanted {
                break;
            }
            let from = times[start];
            let to = from + length;
            if to > end {
                continue;
            }
            let Some(bars) = bars_of(times, from, to) else {
                continue;
            };
            let lead = from.saturating_sub(after);
            let clear = |w: &Window| w.bars.end <= bars.start || w.bars.start >= bars.end;
            if quiet(lead, to)
                && known(lead, to)
                && events.iter().all(clear)
                && controls.iter().all(clear)
            {
                controls.push(Window {
                    kind: Kind::Control,
                    bars,
                    event: None,
                });
            }
        }
    }
    controls.sort_by_key(|w| w.bars.start);
    if controls.len() < wanted {
        // How much of the span the news is known for, outside gaps: often the reason.
        let from = since.map_or(end, |since| since.max(first)).min(end);
        let missed: u64 = gaps
            .iter()
            .map(|&(gap_from, gap_to)| gap_to.min(end).saturating_sub(gap_from.max(from)))
            .sum();
        let known_days = end.saturating_sub(from).saturating_sub(missed) / (24 * HOUR);
        let mut more = vec!["a shorter length"];
        if choice.importance == Importance::Medium {
            more.push("high rather than medium");
        }
        if quiet_of < choice.importance {
            more.push("a control_importance as high as the events'");
        }
        if !choice.market_only {
            more.push("market_only");
        }
        let more = match more.split_last() {
            Some((last, [])) => (*last).to_owned(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        };
        warnings.push(format!(
            "Only {} control window{} could be found of the {wanted} asked. The news is known for {known_days} day{} of this span, outside gaps, and the event windows and headlines take most of it. {} leaves more, or give the windows yourself.",
            controls.len(),
            plural(controls.len()),
            plural(known_days as usize),
            capitalised(&more),
        ));
    }
    if events.is_empty() {
        warnings.push(
            "No event: no headline at that importance falls where a window fits in the bars."
                .into(),
        );
    }
    events.extend(controls);
    Windows {
        windows: events,
        headlines: in_span,
        warnings,
    }
}

const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// How one window went.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowResult {
    pub kind: Kind,
    pub bars: Range<usize>,
    /// Its first bar's open, and the end of its last bar: the `from` and `to` a backtest of the
    /// same bars takes.
    pub from: UnixNanos,
    pub to: UnixNanos,
    /// Its last bar's open.
    pub last_bar: UnixNanos,
    /// Which way it could trade, and the move before it that decided that, with `follow_move`.
    pub way: Direction,
    pub moved: Option<f64>,
    pub event: Option<(UnixNanos, String)>,
    /// The strategy's, as a fraction of the capital.
    pub return_: f64,
    /// Holding the market over the same bars, after any funding.
    pub holding: f64,
    pub trades: usize,
    pub fills: usize,
    /// What a trade still open at the window's end makes of its return: none if none is.
    pub open_at_end: Option<f64>,
}

/// One group's figures.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Group {
    pub windows: usize,
    /// How many made a trade.
    pub traded: usize,
    pub mean_return: f64,
    pub mean_holding: f64,
    /// The strategy's return less holding's, on average.
    pub mean_excess: f64,
    /// The share of its windows the strategy made money in.
    pub profitable: f64,
}

/// What the study found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventStudy {
    pub windows: Vec<WindowResult>,
    pub events: Group,
    pub controls: Group,
    /// The event windows' mean return less the controls': none without both.
    pub difference: Option<f64>,
    /// How often shuffling which windows are events makes a difference at least as big either
    /// way: small means the news likely matters.
    pub p_value: Option<f64>,
    /// The same for the strategy's return over holding's.
    pub excess_difference: Option<f64>,
    pub excess_p_value: Option<f64>,
    /// How many headlines were at the importance asked, for windows from news.
    pub headlines: usize,
    pub warnings: Vec<String>,
}

/// The figures for each group, the difference and its p-value, from `seed`.
#[must_use]
pub fn compare(windows: Vec<WindowResult>, seed: u64) -> EventStudy {
    let group = |kind: Kind| {
        let of: Vec<&WindowResult> = windows.iter().filter(|w| w.kind == kind).collect();
        let n = of.len();
        let mean = |f: &dyn Fn(&WindowResult) -> f64| {
            if n == 0 {
                0.0
            } else {
                of.iter().map(|w| f(w)).sum::<f64>() / n as f64
            }
        };
        Group {
            windows: n,
            traded: of.iter().filter(|w| w.fills > 0).count(),
            mean_return: mean(&|w| w.return_),
            mean_holding: mean(&|w| w.holding),
            mean_excess: mean(&|w| w.return_ - w.holding),
            profitable: mean(&|w| f64::from(u8::from(w.return_ > 0.0))),
        }
    };
    let (events, controls) = (group(Kind::Event), group(Kind::Control));
    let is_event: Vec<bool> = windows.iter().map(|w| w.kind == Kind::Event).collect();
    let returns: Vec<f64> = windows.iter().map(|w| w.return_).collect();
    let excess: Vec<f64> = windows.iter().map(|w| w.return_ - w.holding).collect();
    let mut warnings = Vec::new();
    if events.windows == 0 || controls.windows == 0 {
        warnings.push(format!(
            "{} event and {} control windows: with a group empty there's nothing to compare, so there's no difference or p-value.",
            events.windows, controls.windows
        ));
    } else if events.windows < FEWEST || controls.windows < FEWEST {
        warnings.push(format!(
            "{} event and {} control windows: under {FEWEST} in a group, the p-value means little.",
            events.windows, controls.windows
        ));
    }
    if !windows.is_empty() && events.traded + controls.traded == 0 {
        warnings.push("No window made a trade: the entry rules never came true in any.".into());
    }
    let with_open = windows
        .iter()
        .filter(|w| {
            w.open_at_end
                .is_some_and(|open| open.abs() >= w.return_.abs() / 3.0 && open != 0.0)
        })
        .count();
    if with_open > 0 {
        warnings.push(format!(
            "In {with_open} window{}, a trade still open at the end is a third or more of the result, valued at the last close with no exit cost. close_at_end closes it, paying its costs.",
            plural(with_open)
        ));
    }
    let both = events.windows > 0 && controls.windows > 0;
    EventStudy {
        difference: both.then_some(events.mean_return - controls.mean_return),
        p_value: permutation_p(&returns, &is_event, seed),
        excess_difference: both.then_some(events.mean_excess - controls.mean_excess),
        excess_p_value: permutation_p(&excess, &is_event, seed),
        events,
        controls,
        windows,
        headlines: 0,
        warnings,
    }
}

/// The two-sided permutation p-value of the difference in means between `values` marked `true`
/// and the rest, over [`SHUFFLES`] shuffles: none without at least one of each.
#[must_use]
pub fn permutation_p(values: &[f64], marked: &[bool], seed: u64) -> Option<f64> {
    let n = values.len();
    let k = marked.iter().filter(|&&m| m).count();
    if k == 0 || k == n {
        return None;
    }
    let total: f64 = values.iter().sum();
    let gap = |inside: f64| inside / k as f64 - (total - inside) / (n - k) as f64;
    let observed = gap(values
        .iter()
        .zip(marked)
        .filter(|(_, m)| **m)
        .map(|(v, _)| v)
        .sum())
    .abs();
    let mut rng = StdRng::seed_from_u64(seed);
    let mut order: Vec<usize> = (0..n).collect();
    let mut as_big = 0;
    for _ in 0..SHUFFLES {
        order.shuffle(&mut rng);
        let inside: f64 = order[..k].iter().map(|&i| values[i]).sum();
        // A hair's leeway, so a shuffle that's the same split isn't lost to rounding.
        if gap(inside).abs() >= observed - 1e-12 {
            as_big += 1;
        }
    }
    Some((as_big + 1) as f64 / (SHUFFLES + 1) as f64)
}

/// A study ready to run: the chart's backtest without its bars, and the windows.
pub struct StudyJob {
    pub request: BacktestRequest,
    pub candles: Vec<Candle>,
    pub windows: Windows,
    pub warm_up: usize,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
    /// For the p-values' shuffles.
    pub seed: u64,
    /// Trade each window only the way the market moved before it.
    pub follow_move: Option<FollowMove>,
}

impl StudyJob {
    /// Backtests every window, each warmed up on the bars before it, and compares the groups.
    ///
    /// # Errors
    ///
    /// Returns a message if a backtest fails, or it was stopped.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<EventStudy, String> {
        let capital = self.request.starting_balance;
        let funding = self.request.funding.clone().unwrap_or_default();
        let market: Vec<(u64, f64)> = self.candles.iter().map(|c| (c.ts, c.close)).collect();
        let times: Vec<UnixNanos> = market.iter().map(|&(ts, _)| ts).collect();
        let closes: Vec<f64> = market.iter().map(|&(_, close)| close).collect();
        let moves: Vec<Option<(f64, Direction)>> = match self.follow_move {
            Some(follow) => {
                if self.request.rules.direction != Direction::Both {
                    return Err("Following the move (follow_move) decides which way each window trades, so turn the direction setting off.".into());
                }
                self.windows
                    .windows
                    .iter()
                    .map(|w| follow.before(&times, &closes, &w.bars))
                    .collect()
            }
            None => vec![None; self.windows.windows.len()],
        };
        let unmeasured = self
            .follow_move
            .map_or(0, |_| moves.iter().filter(|m| m.is_none()).count());
        let job = BlockJob {
            directions: moves
                .iter()
                .map(|m| m.map_or(Direction::Both, |(_, way)| way))
                .collect(),
            request: self.request,
            candles: self.candles,
            blocks: self
                .windows
                .windows
                .iter()
                .map(|w| w.bars.clone())
                .collect(),
            warm_up: self.warm_up,
            workers: self.workers,
            stop: self.stop,
        };
        let summary = job.run(progress)?;
        let Windows {
            windows,
            headlines,
            warnings,
        } = self.windows;
        let results = windows
            .into_iter()
            .zip(summary.blocks.iter().zip(&summary.trades))
            .zip(&moves)
            .map(|((window, (block, trades)), moved)| {
                let to = window_end(&times, &window.bars);
                let paid = funding_within(&funding, block.from, to);
                let holding = figures::benchmark(
                    capital,
                    &market[..window.bars.end],
                    window.bars.start,
                    &paid,
                )
                .map_or(0.0, |b| b.return_after_funding);
                WindowResult {
                    from: block.from,
                    to,
                    last_bar: block.to,
                    way: moved.map_or(Direction::Both, |(_, way)| way),
                    moved: moved.map(|(moved, _)| moved),
                    return_: block.stats.return_,
                    holding,
                    trades: trades.len(),
                    fills: block.fills,
                    open_at_end: open_return(trades, capital),
                    kind: window.kind,
                    bars: window.bars,
                    event: window.event,
                }
            })
            .collect();
        let mut study = compare(results, self.seed);
        study.headlines = headlines;
        study.warnings.splice(0..0, warnings);
        if unmeasured > 0 {
            study.warnings.push(format!(
                "{unmeasured} window{} had too few bars before it to measure the move, so traded either way.",
                plural(unmeasured)
            ));
        }
        Ok(study)
    }
}

/// The end of `bars`: the next bar's open, as a backtest of them ends, or a bar's width after the
/// last bar there is.
fn window_end(times: &[UnixNanos], bars: &Range<usize>) -> UnixNanos {
    let width = times
        .len()
        .checked_sub(2)
        .map_or(HOUR, |at| times[at + 1] - times[at]);
    times
        .get(bars.end)
        .copied()
        .unwrap_or_else(|| times[bars.end - 1] + width)
}

/// The funding paid in `[from, to)`: during the last bar too, at whatever moment in it it falls,
/// as a backtest of the same bars counts it.
fn funding_within(funding: &[(u64, f64)], from: UnixNanos, to: UnixNanos) -> Vec<(u64, f64)> {
    funding
        .iter()
        .copied()
        .filter(|&(when, _)| from <= when && when < to)
        .collect()
}

/// What a trade still open at the end made, as a fraction of the capital.
fn open_return(trades: &[RoundTrip], capital: f64) -> Option<f64> {
    trades
        .last()
        .filter(|trade| trade.open && capital > 0.0)
        .map(|trade| trade.profit / capital)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: UnixNanos = 24 * HOUR;
    const START: UnixNanos = 1_700_000_000_000_000_000 / HOUR * HOUR;

    fn hours(count: usize) -> Vec<UnixNanos> {
        (0..count as u64).map(|i| START + i * HOUR).collect()
    }

    fn headline(at: UnixNanos, title: &str) -> Headline {
        Headline {
            published: at,
            seen: at,
            source: "test".into(),
            title: title.into(),
            link: String::new(),
            id: format!("{at}"),
            official: false,
        }
    }

    fn day_choice() -> NewsChoice {
        NewsChoice {
            length_hours: 24,
            after_hours: 2,
            merge_hours: 12,
            ..NewsChoice::default()
        }
    }

    #[test]
    fn news_close_together_is_one_event_and_its_window_starts_after_it() {
        let times = hours(40 * 24);
        let news = [
            headline(START + 5 * DAY, "Fed holds interest rates"),
            headline(START + 5 * DAY + 6 * HOUR, "Inflation surprises"),
            headline(START + 20 * DAY, "Exchange hack drains wallets"),
            headline(START + 30 * DAY, "Bitcoin rallies"),
        ];
        let found = from_news(
            &times,
            0,
            &news,
            &["BTC", "Bitcoin"],
            &[],
            Some(START),
            &day_choice(),
        );
        let events: Vec<&Window> = found
            .windows
            .iter()
            .filter(|w| w.kind == Kind::Event)
            .collect();
        assert_eq!(events.len(), 2, "{:?}", found.windows);
        assert_eq!(events[0].bars, 5 * 24 + 2..6 * 24 + 2);
        assert_eq!(events[1].bars.start, 20 * 24 + 2);
        assert_eq!(found.headlines, 3);
    }

    #[test]
    fn controls_keep_clear_of_news_events_and_gaps() {
        let times = hours(60 * 24);
        let news: Vec<Headline> = (0..6)
            .map(|i| headline(START + (5 + 9 * i) * DAY, "SEC sues an exchange"))
            .collect();
        let gaps = [(START + 30 * DAY, START + 36 * DAY)];
        let choice = NewsChoice {
            controls: Some(6),
            ..day_choice()
        };
        let found = from_news(&times, 0, &news, &[], &gaps, Some(START), &choice);
        let controls: Vec<&Window> = found
            .windows
            .iter()
            .filter(|w| w.kind == Kind::Control)
            .collect();
        assert_eq!(controls.len(), 6, "{:?}", found.warnings);
        let lead = 2 * HOUR;
        for control in &controls {
            let (from, to) = (times[control.bars.start], times[control.bars.start] + DAY);
            assert!(
                news.iter()
                    .all(|h| h.published < from - lead || h.published >= to)
            );
            assert!(gaps.iter().all(|&(a, b)| b <= from - lead || a >= to));
            for other in &found.windows {
                if !std::ptr::eq(other, *control) {
                    assert!(
                        other.bars.end <= control.bars.start
                            || other.bars.start >= control.bars.end
                    );
                }
            }
        }
        // The same seed, the same windows.
        assert_eq!(
            found,
            from_news(&times, 0, &news, &[], &gaps, Some(START), &choice)
        );
    }

    #[test]
    fn too_little_quiet_time_says_so() {
        let times = hours(10 * 24);
        let news: Vec<Headline> = (0..10)
            .map(|i| headline(START + i * DAY, "Rate cut hopes"))
            .collect();
        let found = from_news(&times, 0, &news, &[], &[], Some(START), &day_choice());
        assert!(found.windows.iter().all(|w| w.kind == Kind::Event));
        assert!(
            found
                .warnings
                .iter()
                .any(|w| w.starts_with("Only 0 control")),
            "{:?}",
            found.warnings
        );
    }

    #[test]
    fn stricter_controls_keep_clear_of_medium_headlines_too() {
        let times = hours(60 * 24);
        let mut news: Vec<Headline> = (0..6)
            .map(|i| headline(START + (5 + 9 * i) * DAY, "SEC sues an exchange"))
            .collect();
        let medium: Vec<UnixNanos> = (0..20)
            .map(|i| START + (1 + 3 * i) * DAY + 7 * HOUR)
            .collect();
        news.extend(medium.iter().map(|&at| headline(at, "Bitcoin miners sell")));
        let words = ["bitcoin"];
        assert_eq!(news[6].importance(&words), Importance::Medium);
        let loose = day_choice();
        let strict = NewsChoice {
            control_importance: Some(Importance::Medium),
            ..day_choice()
        };
        let controls = |choice: &NewsChoice| -> Vec<Window> {
            from_news(&times, 0, &news, &words, &[], Some(START), choice)
                .windows
                .into_iter()
                .filter(|w| w.kind == Kind::Control)
                .collect()
        };
        let lead = 2 * HOUR;
        let touches = |w: &Window| {
            let from = times[w.bars.start];
            medium
                .iter()
                .any(|&at| at >= from - lead && at < from + DAY)
        };
        assert!(
            controls(&loose).iter().any(touches),
            "loose controls may hold medium news"
        );
        let strict = controls(&strict);
        assert!(!strict.is_empty());
        assert!(!strict.iter().any(touches));
    }

    #[test]
    fn funding_during_the_last_bar_counts_as_a_backtest_counts_it() {
        let times = hours(10);
        // Paid a few milliseconds after the last bar's open, as exchanges stamp it.
        let late = times[5] + 3_000_000;
        let funding = [(times[1], 0.1), (late, 0.2), (times[6], 0.4)];
        let end = window_end(&times, &(2..6));
        assert_eq!(end, times[6], "the next bar's open, exclusive");
        assert_eq!(funding_within(&funding, times[2], end), vec![(late, 0.2)]);
        assert_eq!(window_end(&times, &(8..10)), times[9] + HOUR);
    }

    #[test]
    fn following_the_move_trades_the_way_the_market_went_before_the_window() {
        let times = hours(12);
        let closes = [
            100.0, 100.0, 100.0, 101.0, 102.0, 102.0, 100.0, 99.0, 99.0, 99.0, 99.4, 99.4,
        ];
        let follow = FollowMove {
            hours: 2,
            threshold: 0.005,
        };
        // Up 2% over the two bars before bar 5, so long only.
        let (moved, way) = follow.before(&times, &closes, &(5..8)).unwrap();
        assert!((moved - 0.02).abs() < 1e-12, "{moved}");
        assert_eq!(way, Direction::LongOnly);
        // Down 2.9% before bar 8.
        assert_eq!(
            follow.before(&times, &closes, &(8..10)).unwrap().1,
            Direction::ShortOnly
        );
        // Up 0.4% before bar 11: under the threshold, either way.
        assert_eq!(
            follow.before(&times, &closes, &(11..12)).unwrap().1,
            Direction::Both
        );
        // Not enough bars before the first ones to measure.
        assert_eq!(follow.before(&times, &closes, &(1..3)), None);
    }

    #[test]
    fn an_empty_group_has_no_difference() {
        let window = |kind: Kind, return_: f64| WindowResult {
            kind,
            bars: 0..2,
            from: 0,
            to: 2,
            last_bar: 1,
            way: Direction::Both,
            moved: None,
            event: None,
            return_,
            holding: 0.0,
            trades: 1,
            fills: 2,
            open_at_end: None,
        };
        let study = compare(
            vec![window(Kind::Event, 0.01), window(Kind::Event, 0.03)],
            0,
        );
        assert_eq!(study.difference, None);
        assert_eq!(study.excess_difference, None);
        assert_eq!(study.p_value, None);
        assert!(
            study.warnings[0].contains("nothing to compare"),
            "{:?}",
            study.warnings
        );
        let both = compare(
            vec![window(Kind::Event, 0.03), window(Kind::Control, 0.01)],
            0,
        );
        assert!((both.difference.unwrap() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn a_clear_split_has_a_small_p_value_and_none_a_large_one() {
        let values: Vec<f64> = (0..40)
            .map(|i| if i < 20 { 1.0 + i as f64 * 0.01 } else { -1.0 })
            .collect();
        let marked: Vec<bool> = (0..40).map(|i| i < 20).collect();
        assert!(permutation_p(&values, &marked, 1).unwrap() < 0.001);
        let mixed: Vec<bool> = (0..40).map(|i| i % 2 == 0).collect();
        let same: Vec<f64> = (0..40).map(|i| (i / 2) as f64).collect();
        assert!(permutation_p(&same, &mixed, 1).unwrap() > 0.5);
        assert_eq!(permutation_p(&values, &[true; 40], 1), None);
    }
}

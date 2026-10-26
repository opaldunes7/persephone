//! Asks a language model for the next day's position, once a day: at the first bar of each
//! UTC day it sends the last few daily closes and goes LONG, SHORT or FLAT as told.
//!
//! The app asks before letting it call a model. Replies are recorded, so rerunning the same
//! backtest is free and gives the same trades. Mind the report's look-ahead warning: a model
//! released after the bars it's asked about may already know how they turned out.

use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, ai, export_strategy};

/// Change to another Claude model, `openai/<model>` or `ollama/<model>`.
const MODEL: &str = "claude-opus-5-5";
const DAY: u64 = 86_400_000_000_000;
const SYSTEM: &str = "You are the trading signal in a backtest. Given daily closing prices, \
reply with exactly one word: LONG, SHORT or FLAT, for the next day.";

struct LlmDaily {
    lookback: usize,
    /// The close of each finished day, oldest first.
    daily: Vec<f64>,
    day: Option<u64>,
    last_close: f64,
}

impl Strategy for LlmDaily {
    fn describe() -> StrategyInfo {
        StrategyInfo::new("LLM Daily (plugin)", "Asks a language model once a day")
            .param(Param::period("Lookback days", 10))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        Ok(Self {
            lookback: params[0] as usize,
            daily: Vec::new(),
            day: None,
            last_close: 0.0,
        })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        let day = bar.ts / DAY;
        let new_day = self.day.is_some_and(|d| d != day);
        if new_day {
            self.daily.push(self.last_close);
        }
        self.day = Some(day);
        self.last_close = bar.close;
        if !new_day || self.daily.len() < self.lookback {
            return None;
        }

        let recent = &self.daily[self.daily.len() - self.lookback..];
        let closes: Vec<String> = recent.iter().map(|c| format!("{c}")).collect();
        let prompt = format!("Daily closes, oldest first: {}", closes.join(", "));
        let request = ai::Request::new(MODEL, prompt)
            .system(SYSTEM)
            .effort(ai::Effort::Low);
        // No answer (not allowed yet, offline, out of calls): keep the position.
        let reply = ai::complete(&request).ok()?;
        match reply
            .split_whitespace()
            .next()?
            .trim_matches(|c: char| !c.is_alphabetic())
            .to_uppercase()
            .as_str()
        {
            "LONG" => Some(1),
            "SHORT" => Some(-1),
            "FLAT" => Some(0),
            _ => None,
        }
    }
}

export_strategy!(LlmDaily);

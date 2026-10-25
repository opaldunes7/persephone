//! Indicator series ready to draw: values per bar plus how to show them. Where the values
//! come from (built-ins, plugins) is up to the caller.

use iced::Color;
pub use persephone_core::indicator::{OutputStyle, Placement};

#[derive(Clone, Debug)]
pub struct StudyLine {
    pub name: String,
    /// One value per candle; `NaN` where there is none (e.g. while warming up).
    pub values: Vec<f64>,
    /// Line color, or the color of positive bars for a histogram.
    pub color: Color,
    pub style: OutputStyle,
}

#[derive(Clone, Debug)]
pub struct Study {
    /// Legend label, e.g. "EMA 21".
    pub label: String,
    pub placement: Placement,
    pub lines: Vec<StudyLine>,
    /// Shades the area between two lines (upper, lower).
    pub fill: Option<(usize, usize, Color)>,
    /// Dashed reference levels (e.g. RSI 30/70).
    pub levels: Vec<f64>,
    /// A fixed scale for its pane; `None` fits the visible values.
    pub range: Option<(f64, f64)>,
    /// Shown in the legend instead of values when the study couldn't be computed.
    pub error: Option<String>,
}

impl Study {
    /// Every line's values, for fitting scales.
    pub(crate) fn series(&self) -> Vec<&[f64]> {
        self.lines.iter().map(|l| l.values.as_slice()).collect()
    }
}

//! Turns a tile's indicator choices into chart studies: values from the engine, colors and
//! styles from TradingView's defaults.

use iced::Color;
use persephone_chart::{FillMark, Placement, Study, StudyLine, Trades, theme};
use persephone_core::{
    Candle,
    indicator::{BuiltIn, IndicatorSpec, OutputStyle},
};
use persephone_engine::BacktestReport;

const BLUE: Color = Color::from_rgb8(0x29, 0x62, 0xff);
const ORANGE: Color = Color::from_rgb8(0xff, 0x6d, 0x00);
const PURPLE: Color = Color::from_rgb8(0x7e, 0x57, 0xc2);
const RED: Color = Color::from_rgb8(0xf2, 0x36, 0x45);
const HISTOGRAM: Color = Color::from_rgba8(0x26, 0xa6, 0x9a, 0.6);
const EQUITY: Color = Color::from_rgb8(0x42, 0xa5, 0xf5);

/// Computes every indicator over `candles`, in order. An indicator that can't be computed
/// (e.g. MACD with fast ≥ slow) still gets a study, carrying the error for its legend.
pub fn build(specs: &[IndicatorSpec], candles: &[Candle]) -> Vec<Study> {
    let mut palette = 0;
    specs
        .iter()
        .map(|spec| {
            let colors = colors(spec.kind, &mut palette);
            let mut study = Study {
                label: spec.to_string(),
                placement: spec.kind.placement(),
                lines: Vec::new(),
                fill: spec.kind.fill().map(|(upper, lower)| {
                    (
                        upper,
                        lower,
                        Color {
                            a: 0.08,
                            ..colors[upper]
                        },
                    )
                }),
                levels: spec.kind.levels().to_vec(),
                range: spec.kind.range(),
                error: None,
            };
            match persephone_engine::indicators::compute(spec, candles) {
                Ok(outputs) => {
                    study.lines = outputs
                        .into_iter()
                        .zip(spec.kind.outputs())
                        .zip(colors)
                        .map(|((values, output), color)| StudyLine {
                            name: output.name.into(),
                            values,
                            color,
                            style: output.style,
                        })
                        .collect();
                }
                Err(error) => study.error = Some(error),
            }
            study
        })
        .collect()
}

/// The strategy's equity curve as a pane, with the starting balance as a dashed level.
pub fn equity(report: &BacktestReport) -> Study {
    Study {
        label: "Equity".into(),
        placement: Placement::Pane,
        lines: vec![StudyLine {
            name: "Equity".into(),
            values: report.equity.clone(),
            color: EQUITY,
            style: OutputStyle::Line,
        }],
        fill: None,
        levels: vec![report.starting_balance],
        range: None,
        error: None,
    }
}

/// The strategy's fills as chart markers and holding periods.
pub fn trades(report: &BacktestReport, bars: usize) -> Trades {
    let fills: Vec<FillMark> = report
        .fills
        .iter()
        .map(|f| FillMark {
            bar: f.bar,
            side: f.side,
            price: f.price,
            qty: f.qty,
        })
        .collect();
    Trades::from_fills(&fills, bars)
}

/// One color per output. Moving averages and channels take the next palette color so
/// several of them on one chart stay distinguishable.
fn colors(kind: BuiltIn, next: &mut usize) -> Vec<Color> {
    let mut take = || {
        let color = theme::STUDY_PALETTE[*next % theme::STUDY_PALETTE.len()];
        *next += 1;
        color
    };
    match kind {
        BuiltIn::Sma | BuiltIn::Ema => vec![take()],
        BuiltIn::Bollinger | BuiltIn::Donchian => {
            let band = take();
            vec![band, ORANGE, band]
        }
        BuiltIn::Rsi => vec![PURPLE],
        BuiltIn::Macd => vec![HISTOGRAM, BLUE, ORANGE],
        BuiltIn::Stochastic => vec![BLUE, ORANGE],
        BuiltIn::Atr => vec![RED],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(n: usize) -> Vec<Candle> {
        (0..n)
            .map(|i| {
                let p = 100.0 + (i as f64 / 9.0).sin() * 5.0;
                Candle {
                    ts: i as u64,
                    open: p,
                    high: p + 1.0,
                    low: p - 1.0,
                    close: p + 0.3,
                    volume: 1.0,
                }
            })
            .collect()
    }

    #[test]
    fn builds_one_study_per_spec_in_order() {
        let specs: Vec<_> = BuiltIn::ALL.into_iter().map(IndicatorSpec::new).collect();
        let studies = build(&specs, &candles(300));
        assert_eq!(studies.len(), specs.len());
        for (study, spec) in studies.iter().zip(&specs) {
            assert_eq!(study.label, spec.to_string());
            assert_eq!(study.lines.len(), spec.kind.outputs().len());
            assert!(study.error.is_none());
        }
        assert_eq!(
            studies[4].placement,
            Placement::Pane,
            "RSI gets its own pane"
        );
        assert_eq!(studies[4].range, Some((0.0, 100.0)));
        assert!(studies[2].fill.is_some(), "Bollinger is shaded");
    }

    #[test]
    fn moving_averages_get_distinct_colors() {
        let specs = [
            IndicatorSpec::new(BuiltIn::Ema),
            IndicatorSpec::new(BuiltIn::Sma),
        ];
        let studies = build(&specs, &candles(100));
        assert_ne!(studies[0].lines[0].color, studies[1].lines[0].color);
    }

    #[test]
    fn bad_parameters_show_up_as_errors() {
        let spec = IndicatorSpec::with_params(BuiltIn::Macd, &[30.0, 10.0, 9.0]);
        let studies = build(&[spec], &candles(100));
        assert!(
            studies[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("fast")
        );
        assert!(studies[0].lines.is_empty());
    }
}

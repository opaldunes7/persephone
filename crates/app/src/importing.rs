//! Importing CSV tick files dropped on a chart: the same detection and import as
//! `persephone-cli import`, run off the UI thread, with progress.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use iced::{
    Task,
    futures::{SinkExt, channel::mpsc},
};
use persephone_data::{
    import::{self, Format, MarketOptions, Overrides},
    store::Store,
};

use crate::{Message, on_thread};

/// How often progress reaches the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A symbol guessed from a file name: the leading letters and digits, uppercased
/// (`EURUSD-2014-01.csv` → `EURUSD`).
pub fn guess_symbol(path: &Path) -> String {
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    stem.chars()
        .take_while(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .take(32)
        .collect()
}

/// Works out `path`'s layout on a worker thread.
pub fn detect(path: PathBuf) -> Task<Message> {
    let job = path.clone();
    Task::perform(
        on_thread(move || {
            let sample = import::sample(&job).map_err(|e| format!("{e:#}"))?;
            import::detect(&sample, &Overrides::default())
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"))
        }),
        move |result| Message::Detected(path.clone(), result),
    )
}

/// Imports `files` on a worker thread, reporting progress as it goes.
pub fn run(
    store: Store,
    files: Vec<PathBuf>,
    format: Format,
    options: MarketOptions,
) -> Task<Message> {
    Task::stream(iced::stream::channel(
        16,
        async move |mut output: mpsc::Sender<Message>| {
            let (done, finished) = iced::futures::channel::oneshot::channel();
            let mut progress = output.clone();
            std::thread::spawn(move || {
                let mut shown = Instant::now();
                let result = import::import(&store, &files, &format, &options, &mut |p| {
                    if shown.elapsed() >= PROGRESS_EVERY {
                        shown = Instant::now();
                        let _ = progress.try_send(Message::ImportProgress(*p));
                    }
                    true
                });
                let _ = done.send(result.map(Arc::new).map_err(|e| format!("{e:#}")));
            });
            let result = finished
                .await
                .unwrap_or_else(|_| Err("the import stopped unexpectedly".into()));
            let _ = output.send(Message::Imported(result)).await;
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_symbols_from_file_names() {
        let guess = |name: &str| guess_symbol(Path::new(name));
        assert_eq!(guess("/data/EURUSD-2014-01.csv"), "EURUSD");
        assert_eq!(guess("BTCUSDT-trades-2024-01.zip"), "BTCUSDT");
        assert_eq!(guess("bitstampUSD.csv.gz"), "BITSTAMPUSD");
        assert_eq!(guess("_odd.csv"), "");
    }
}

//! Saves the tile layout (and what each tile shows) so the next launch picks up where you
//! left off. Stored as RON in the platform data folder, e.g.
//! `~/Library/Application Support/persephone/layout.ron` on macOS.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use persephone_core::Timeframe;
use persephone_tiling::{Layout, TileId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub layout: Layout,
    pub tiles: Vec<SavedTile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTile {
    pub id: TileId,
    pub symbol: String,
    /// Timeframe label, e.g. "1m".
    pub timeframe: String,
    /// Missing in layouts saved before indicators existed.
    #[serde(default)]
    pub indicators: Vec<SavedIndicator>,
    /// Missing in layouts saved before strategies existed.
    #[serde(default)]
    pub strategy: Option<SavedStrategy>,
}

/// A built-in strategy by id (e.g. "ema-cross") with its parameters. Unknown ids are skipped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedStrategy {
    pub kind: String,
    pub params: Vec<f64>,
}

/// A built-in indicator by id (e.g. "ema") with its parameters. Unknown ids are skipped when
/// loading, so older builds can open newer layouts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedIndicator {
    pub kind: String,
    pub params: Vec<f64>,
}

impl Saved {
    /// A saved layout is usable when the tree is valid and every tile has exactly one entry
    /// with a known timeframe.
    pub fn is_valid(&self) -> bool {
        let in_layout: BTreeSet<TileId> = self.layout.tiles().into_iter().collect();
        let described: BTreeSet<TileId> = self.tiles.iter().map(|t| t.id).collect();
        self.layout.is_valid()
            && in_layout == described
            && described.len() == self.tiles.len()
            && self
                .tiles
                .iter()
                .all(|t| t.timeframe.parse::<Timeframe>().is_ok() && !t.symbol.is_empty())
    }
}

pub fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("persephone").join("layout.ron"))
}

/// Reads a saved layout. Missing, unreadable or invalid files all mean "start fresh".
pub fn load(path: &Path) -> Option<Saved> {
    let text = fs::read_to_string(path).ok()?;
    match ron::from_str::<Saved>(&text) {
        Ok(saved) if saved.is_valid() => Some(saved),
        Ok(_) => {
            eprintln!("persephone: ignoring invalid layout in {}", path.display());
            None
        }
        Err(e) => {
            eprintln!(
                "persephone: ignoring unreadable layout in {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// Writes atomically (temp file + rename) so a crash never leaves half a layout behind.
pub fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = ron::ser::to_string_pretty(saved, ron::ser::PrettyConfig::default())
        .map_err(io::Error::other)?;
    let tmp = path.with_extension("ron.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use persephone_tiling::Rect;

    use super::*;

    fn saved() -> Saved {
        let mut layout = Layout::new();
        layout.insert(Rect::new(0.0, 0.0, 1600.0, 900.0)).unwrap();
        Saved {
            layout,
            tiles: vec![
                SavedTile {
                    id: 0,
                    symbol: "BTCUSDT".into(),
                    timeframe: "1m".into(),
                    indicators: vec![SavedIndicator {
                        kind: "ema".into(),
                        params: vec![21.0],
                    }],
                    strategy: Some(SavedStrategy {
                        kind: "ema-cross".into(),
                        params: vec![10.0, 30.0, 10.0],
                    }),
                },
                SavedTile {
                    id: 1,
                    symbol: "ETHUSDT".into(),
                    timeframe: "4h".into(),
                    indicators: Vec::new(),
                    strategy: None,
                },
            ],
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("persephone-test-{}-{name}", std::process::id()))
            .join("layout.ron")
    }

    #[test]
    fn round_trips_through_disk() {
        let path = temp_path("round-trip");
        save(&path, &saved()).unwrap();
        assert_eq!(load(&path), Some(saved()));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_or_broken_files_start_fresh() {
        let path = temp_path("broken");
        assert_eq!(load(&path), None);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not ron at all").unwrap();
        assert_eq!(load(&path), None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn layouts_saved_before_indicators_and_strategies_still_load() {
        let path = temp_path("pre-indicators");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut old = ron::ser::to_string(&saved()).unwrap();
        old = old
            .replace(",indicators:[(kind:\"ema\",params:[21.0])]", "")
            .replace(",indicators:[]", "")
            .replace(
                ",strategy:Some((kind:\"ema-cross\",params:[10.0,30.0,10.0]))",
                "",
            )
            .replace(",strategy:None", "");
        assert!(
            !old.contains("indicators") && !old.contains("strategy"),
            "{old}"
        );
        fs::write(&path, old).unwrap();
        let loaded = load(&path).expect("still loads");
        assert!(
            loaded
                .tiles
                .iter()
                .all(|t| t.indicators.is_empty() && t.strategy.is_none())
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn tiles_must_match_the_layout() {
        let mut missing = saved();
        missing.tiles.pop();
        assert!(!missing.is_valid());

        let mut bad_timeframe = saved();
        bad_timeframe.tiles[0].timeframe = "7m".into();
        assert!(!bad_timeframe.is_valid());

        assert!(saved().is_valid());
    }
}

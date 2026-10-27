//! Saves the tile layout (and what each tile shows) so the next launch picks up where you
//! left off. Stored as RON in the platform data folder, e.g.
//! `~/Library/Application Support/persephone/layout.ron` on macOS.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use persephone_core::BarSpec;
use persephone_tiling::{Layout, TileId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The workspace on screen.
    pub layout: Layout,
    pub tiles: Vec<SavedTile>,
    /// Its number, 1 to 9; missing in layouts saved before workspaces existed.
    #[serde(default = "first_workspace")]
    pub workspace: u8,
    /// The other workspaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<SavedWorkspace>,
}

/// A workspace that wasn't on screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub number: u8,
    pub layout: Layout,
    pub tiles: Vec<SavedTile>,
}

const fn first_workspace() -> u8 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTile {
    pub id: TileId,
    /// The venue of a market in the local store; missing for Binance markets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    pub symbol: String,
    /// How its bars are cut: a timeframe ("1m"), ticks ("1000t") or volume ("2.5v").
    pub timeframe: String,
    /// Missing in layouts saved before indicators existed.
    #[serde(default)]
    pub indicators: Vec<SavedIndicator>,
    /// Missing in layouts saved before strategies existed.
    #[serde(default)]
    pub strategy: Option<SavedStrategy>,
    /// The Active list has hidden the market's drawings on this chart.
    #[serde(default, skip_serializing_if = "is_false")]
    pub drawings_hidden: bool,
    /// The Active list has hidden the volume bars on this chart.
    #[serde(default, skip_serializing_if = "is_false")]
    pub volume_hidden: bool,
}

/// A built-in strategy by id (e.g. "ema-cross") with its parameters. Unknown ids are skipped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedStrategy {
    pub kind: String,
    pub params: Vec<f64>,
    /// Switched off in the Active list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

/// A built-in indicator by id (e.g. "ema") with its parameters. Unknown ids are skipped when
/// loading, so older builds can open newer layouts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedIndicator {
    pub kind: String,
    pub params: Vec<f64>,
    /// Switched off in the Active list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes fields by reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

impl Saved {
    /// A saved layout is usable when every workspace's tree is valid, every tile has exactly
    /// one entry with a known timeframe, workspaces are numbered 1 to 9 once each, and no two
    /// tiles anywhere share an id.
    pub fn is_valid(&self) -> bool {
        let mut numbers = BTreeSet::from([self.workspace]);
        let mut ids = BTreeSet::new();
        let workspaces = std::iter::once((self.workspace, &self.layout, &self.tiles))
            .chain(self.others.iter().map(|w| (w.number, &w.layout, &w.tiles)));
        let mut count = 0;
        for (number, layout, tiles) in workspaces {
            count += 1;
            numbers.insert(number);
            ids.extend(tiles.iter().map(|t| t.id));
            if !(1..=9).contains(&number) || !workspace_is_valid(layout, tiles) {
                return false;
            }
        }
        let tiles = self.tiles.len() + self.others.iter().map(|w| w.tiles.len()).sum::<usize>();
        numbers.len() == count && ids.len() == tiles
    }
}

fn workspace_is_valid(layout: &Layout, tiles: &[SavedTile]) -> bool {
    let in_layout: BTreeSet<TileId> = layout.tiles().into_iter().collect();
    let described: BTreeSet<TileId> = tiles.iter().map(|t| t.id).collect();
    layout.is_valid()
        && in_layout == described
        && described.len() == tiles.len()
        && tiles
            .iter()
            .all(|t| t.timeframe.parse::<BarSpec>().is_ok() && !t.symbol.is_empty())
}

/// Drawings by market (see [`drawing_key`](crate::drawing_key)), kept next to the layout.
pub type Drawings = std::collections::BTreeMap<String, Vec<persephone_chart::Drawing>>;

/// Reads saved drawings; a missing or unreadable file means none.
#[must_use]
pub fn load_drawings(path: &Path) -> Drawings {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| ron::from_str(&text).ok())
        .unwrap_or_default()
}

/// # Errors
///
/// Returns an error if the file can't be written.
pub fn save_drawings(path: &Path, drawings: &Drawings) -> io::Result<()> {
    let text = ron::ser::to_string_pretty(drawings, ron::ser::PrettyConfig::default())
        .map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)
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
            workspace: 1,
            others: Vec::new(),
            layout,
            tiles: vec![
                SavedTile {
                    id: 0,
                    local: None,
                    symbol: "BTCUSDT".into(),
                    timeframe: "1m".into(),
                    indicators: vec![SavedIndicator {
                        kind: "ema".into(),
                        params: vec![21.0],
                        hidden: true,
                    }],
                    strategy: Some(SavedStrategy {
                        kind: "ema-cross".into(),
                        params: vec![10.0, 30.0, 10.0],
                        hidden: false,
                    }),
                    drawings_hidden: false,
                    volume_hidden: false,
                },
                SavedTile {
                    id: 1,
                    local: Some("TRUEFX".into()),
                    symbol: "EURUSD".into(),
                    timeframe: "4h".into(),
                    indicators: Vec::new(),
                    strategy: None,
                    drawings_hidden: true,
                    volume_hidden: false,
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
            .replace(",indicators:[(kind:\"ema\",params:[21.0],hidden:true)]", "")
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
    fn workspaces_round_trip_and_old_files_land_on_the_first() {
        let path = temp_path("workspaces");
        let mut saved = saved();
        saved.workspace = 3;
        saved.others = vec![SavedWorkspace {
            number: 1,
            layout: Layout::starting_at(10),
            tiles: vec![SavedTile {
                id: 10,
                ..saved.tiles[1].clone()
            }],
        }];
        assert!(saved.is_valid());
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), Some(saved.clone()));

        let old = ron::to_string(&saved.layout).unwrap();
        let text = format!("(layout: {old}, tiles: [])");
        let parsed: Saved = ron::from_str(&text).unwrap();
        assert_eq!((parsed.workspace, parsed.others.len()), (1, 0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn workspaces_must_be_numbered_once_and_never_share_ids() {
        let mut twice = saved();
        twice.others = vec![SavedWorkspace {
            number: 1,
            layout: Layout::starting_at(10),
            tiles: vec![SavedTile {
                id: 10,
                ..twice.tiles[1].clone()
            }],
        }];
        assert!(!twice.is_valid(), "workspace 1 twice");

        let mut clash = saved();
        clash.workspace = 2;
        clash.others = vec![SavedWorkspace {
            number: 1,
            layout: Layout::new(),
            tiles: vec![SavedTile {
                id: 0,
                ..clash.tiles[1].clone()
            }],
        }];
        assert!(!clash.is_valid(), "tile 0 in two workspaces");

        let mut out_of_range = saved();
        out_of_range.workspace = 10;
        assert!(!out_of_range.is_valid());
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

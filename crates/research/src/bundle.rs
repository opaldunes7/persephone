//! Everything worth moving between machines, as one JSON file: settings, saved results, the layout,
//! drawings and which plugins are in use, and full runs if asked for. The app and Lab both read and
//! write it here, so a file from one works in the other.
//!
//! What's left out on purpose: API keys and what AI models a plugin may use, so a shared file can't
//! leak a key or grant spending; plugins' code, as importing code from a file is a risk of its own;
//! and market data, which is large and comes back with `export_data` or a download.

use std::{
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    results::{SavedResult, load_results},
    settings::Settings,
};

/// What says a file is one of these.
pub const FORMAT: &str = "persephone-bundle";
/// The newest version this can read. Raise it when an old reader would get a file wrong.
pub const VERSION: u32 = 1;

/// One chart's setup: its settings, and its strategy with its parameters' values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartSetup {
    pub name: String,
    /// As a strategy is written anywhere else, so Lab's calls can use it too.
    #[serde(default)]
    pub strategy: Option<Value>,
    #[serde(default)]
    pub settings: Option<Settings>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    /// Unix milliseconds.
    pub exported_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<ChartSetup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SavedResult>,
    /// The layout and drawings exactly as the app stores them, so nothing's lost on the way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_ron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawings_ron: Option<String>,
    /// The names of the plugin files in use, to say which are missing where it's imported. Not
    /// their code, and not where a program over a socket listens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    /// Full runs, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<SavedRun>,
}

/// A run as Lab saves it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedRun {
    pub id: String,
    pub run: Value,
    /// Its short summary, which listing runs reads.
    #[serde(default)]
    pub brief: Value,
}

/// Where everything lives.
#[derive(Clone, Debug)]
pub struct Places {
    pub layout: PathBuf,
    pub drawings: PathBuf,
    pub results: PathBuf,
    pub runs: PathBuf,
    pub plugins: PathBuf,
}

impl Places {
    /// Everything under the app's data folder.
    #[must_use]
    pub fn under(data: &Path) -> Self {
        Self {
            layout: data.join("layout.ron"),
            drawings: data.join("drawings.ron"),
            results: crate::results::results_dir(data),
            runs: data.join("lab").join("runs"),
            plugins: data.join("plugins"),
        }
    }
}

/// What importing did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub results_added: usize,
    /// Already here under the same id, which are kept as they are.
    pub results_skipped: usize,
    pub runs_added: usize,
    pub runs_skipped: usize,
    /// Plugins the bundle names that aren't installed here.
    pub plugins_missing: Vec<String>,
}

impl Imported {
    /// A line saying so.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut said = format!("{} results added", self.results_added);
        if self.results_skipped > 0 {
            said += &format!(", {} already here", self.results_skipped);
        }
        if self.runs_added + self.runs_skipped > 0 {
            said += &format!(
                "; {} runs added, {} already here",
                self.runs_added, self.runs_skipped
            );
        }
        if !self.plugins_missing.is_empty() {
            said += &format!("; plugins to install: {}", self.plugins_missing.join(", "));
        }
        said
    }
}

/// An id from a file becomes a file name, so it mustn't lead anywhere else.
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("`{id}` in the file isn't a usable id"));
    }
    Ok(())
}

/// Written aside and moved, so nothing ever reads half a file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)
        .and_then(|()| fs::rename(&partial, path))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

impl Bundle {
    fn empty(exported_at: u64) -> Self {
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            exported_at,
            chart: None,
            results: Vec::new(),
            layout_ron: None,
            drawings_ron: None,
            plugins: Vec::new(),
            runs: Vec::new(),
        }
    }

    /// One chart's setup.
    #[must_use]
    pub fn of_chart(chart: ChartSetup, exported_at: u64) -> Self {
        Self {
            chart: Some(chart),
            ..Self::empty(exported_at)
        }
    }

    /// Everything at `places`, and full runs if asked for, as they can be large.
    ///
    /// # Errors
    ///
    /// Returns a message if a saved run can't be read.
    pub fn gather(places: &Places, include_runs: bool, exported_at: u64) -> Result<Self, String> {
        let mut plugins: Vec<String> = fs::read_dir(&places.plugins)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                    .filter(|name| !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        plugins.sort();
        Ok(Self {
            results: load_results(&places.results),
            layout_ron: fs::read_to_string(&places.layout).ok(),
            drawings_ron: fs::read_to_string(&places.drawings).ok(),
            plugins,
            runs: if include_runs {
                read_runs(&places.runs)?
            } else {
                Vec::new()
            },
            ..Self::empty(exported_at)
        })
    }

    /// Writes it, replacing what's there only if told to.
    ///
    /// # Errors
    ///
    /// Returns a message if the file exists and mayn't be replaced, or can't be written.
    pub fn write(&self, path: &Path, replace: bool) -> Result<(), String> {
        if !replace && path.exists() {
            return Err(format!("{} already exists", path.display()));
        }
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_new(path, text.as_bytes())
    }

    /// Reads one, refusing what isn't one or came from a newer version than this understands.
    ///
    /// # Errors
    ///
    /// Returns a message saying which.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// See [`Self::read`].
    ///
    /// # Errors
    ///
    /// Returns a message saying what's wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("that isn't JSON: {e}"))?;
        if value.get("format").and_then(Value::as_str) != Some(FORMAT) {
            return Err("that isn't a Persephone export".into());
        }
        let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version > u64::from(VERSION) {
            return Err(format!(
                "that's from a newer Persephone (version {version}, and this reads up to {VERSION}). Update first."
            ));
        }
        serde_json::from_value(value).map_err(|e| format!("that export is damaged: {e}"))
    }

    /// Adds its results and runs at `places`. What's already there under the same id stays as it
    /// is, as nothing here overwrites what you have. The chart, layout and drawings are for the app
    /// to apply, so it can ask first.
    ///
    /// # Errors
    ///
    /// Returns a message before changing anything if any id in it is unusable, or if a file can't
    /// be written.
    pub fn add_to(&self, places: &Places) -> Result<Imported, String> {
        for result in &self.results {
            check_id(&result.id)?;
        }
        for run in &self.runs {
            check_id(&run.id)?;
        }
        let mut imported = Imported::default();
        if !self.results.is_empty() {
            fs::create_dir_all(&places.results).map_err(|e| e.to_string())?;
        }
        for result in &self.results {
            let path = places.results.join(format!("{}.json", result.id));
            if path.exists() {
                imported.results_skipped += 1;
                continue;
            }
            let text = serde_json::to_string_pretty(result).map_err(|e| e.to_string())?;
            write_new(&path, text.as_bytes())?;
            imported.results_added += 1;
        }
        if !self.runs.is_empty() {
            fs::create_dir_all(&places.runs).map_err(|e| e.to_string())?;
        }
        for saved in &self.runs {
            let path = places.runs.join(format!("{}.json.gz", saved.id));
            if path.exists() {
                imported.runs_skipped += 1;
                continue;
            }
            let mut packed = GzEncoder::new(Vec::new(), Compression::fast());
            let bytes = packed
                .write_all(saved.run.to_string().as_bytes())
                .and_then(|()| packed.finish())
                .map_err(|e| e.to_string())?;
            write_new(&path, &bytes)?;
            if !saved.brief.is_null() {
                let brief = places.runs.join(format!("{}.summary", saved.id));
                write_new(&brief, saved.brief.to_string().as_bytes())?;
            }
            imported.runs_added += 1;
        }
        imported.plugins_missing = self
            .plugins
            .iter()
            .filter(|name| !places.plugins.join(name).exists())
            .cloned()
            .collect();
        Ok(imported)
    }
}

fn read_runs(dir: &Path) -> Result<Vec<SavedRun>, String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut runs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json.gz")) else {
            continue;
        };
        let file = File::open(entry.path()).map_err(|e| e.to_string())?;
        let mut text = String::new();
        GzDecoder::new(file)
            .read_to_string(&mut text)
            .map_err(|e| format!("couldn't read the run {id}: {e}"))?;
        let run =
            serde_json::from_str(&text).map_err(|e| format!("couldn't read the run {id}: {e}"))?;
        let brief = fs::read_to_string(dir.join(format!("{id}.summary")))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        runs.push(SavedRun {
            id: id.to_owned(),
            run,
            brief,
        });
    }
    runs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("persephone-bundle-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn result(id: &str) -> SavedResult {
        SavedResult {
            id: id.into(),
            name: format!("Saved {id}"),
            note: "a note".into(),
            strategy: json!({"builtin": "ema-cross"}),
            settings: Some(Settings::default()),
            summary: json!({"net_return": 0.1}),
            saved_at: 5,
        }
    }

    fn filled(name: &str) -> Places {
        let data = folder(name);
        let places = Places::under(&data);
        fs::create_dir_all(&places.results).unwrap();
        fs::create_dir_all(&places.runs).unwrap();
        fs::create_dir_all(&places.plugins).unwrap();
        fs::write(&places.layout, "(layout)").unwrap();
        fs::write(&places.drawings, "{}").unwrap();
        fs::write(places.plugins.join("mine.wasm"), "code").unwrap();
        fs::write(
            data.join("ai.json"),
            r#"{"anthropic_api_key": "SECRET-KEY"}"#,
        )
        .unwrap();
        Bundle {
            results: vec![result("one"), result("two")],
            runs: vec![SavedRun {
                id: "run-1".into(),
                run: json!({"equity": [1.0, 2.0]}),
                brief: json!({"run_id": "run-1"}),
            }],
            ..Bundle::empty(1)
        }
        .add_to(&places)
        .unwrap();
        places
    }

    #[test]
    fn what_goes_out_comes_back_exactly() {
        let from = filled("from");
        let bundle = Bundle::gather(&from, true, 9).unwrap();
        let file = folder("file").join("all.json");
        bundle.write(&file, false).unwrap();
        let read = Bundle::read(&file).unwrap();
        assert_eq!(read, bundle);
        assert_eq!(read.layout_ron.as_deref(), Some("(layout)"));
        assert_eq!(read.plugins, ["mine.wasm"]);

        let to = Places::under(&folder("to"));
        let done = read.add_to(&to).unwrap();
        assert_eq!((done.results_added, done.runs_added), (2, 1));
        assert_eq!(done.plugins_missing, ["mine.wasm"]);
        assert_eq!(load_results(&to.results), load_results(&from.results));
        assert_eq!(Bundle::gather(&to, true, 9).unwrap().runs, bundle.runs);

        // Again, nothing changes and what's there is kept.
        let again = read.add_to(&to).unwrap();
        assert_eq!((again.results_added, again.results_skipped), (0, 2));
        assert_eq!((again.runs_added, again.runs_skipped), (0, 1));
    }

    #[test]
    fn keys_and_plugin_code_never_go_in_and_runs_only_when_asked() {
        let places = filled("keys");
        let text = serde_json::to_string(&Bundle::gather(&places, true, 1).unwrap()).unwrap();
        assert!(!text.contains("SECRET-KEY"));
        assert!(!text.contains("code"));
        assert!(Bundle::gather(&places, false, 1).unwrap().runs.is_empty());
    }

    #[test]
    fn a_file_that_isnt_right_says_so_and_changes_nothing() {
        assert!(Bundle::parse("nope").unwrap_err().contains("JSON"));
        assert!(
            Bundle::parse("{}")
                .unwrap_err()
                .contains("isn't a Persephone")
        );
        let newer = json!({"format": FORMAT, "version": VERSION + 1}).to_string();
        assert!(Bundle::parse(&newer).unwrap_err().contains("newer"));

        // An id that would write somewhere else stops all of it, before anything's written.
        let to = Places::under(&folder("escape"));
        let mut bundle = Bundle::empty(1);
        bundle.results = vec![result("fine"), result("../../escaped")];
        assert!(bundle.add_to(&to).unwrap_err().contains("usable id"));
        assert!(load_results(&to.results).is_empty());
        let mut bundle = Bundle::empty(1);
        bundle.runs = vec![SavedRun {
            id: "/tmp/evil".into(),
            run: json!({}),
            brief: Value::Null,
        }];
        assert!(bundle.add_to(&to).unwrap_err().contains("usable id"));

        // It won't replace a file unless told to.
        let file = folder("replace").join("all.json");
        Bundle::empty(1).write(&file, false).unwrap();
        assert!(Bundle::empty(2).write(&file, false).is_err());
        Bundle::empty(2).write(&file, true).unwrap();
        assert_eq!(Bundle::read(&file).unwrap().exported_at, 2);
    }

    #[test]
    fn a_files_unknown_parts_dont_stop_it_being_read() {
        // Older readers ignore what's new, within a version.
        let text = json!({"format": FORMAT, "version": 1, "exported_at": 1, "extra": true});
        assert!(Bundle::parse(&text.to_string()).is_ok());
    }
}

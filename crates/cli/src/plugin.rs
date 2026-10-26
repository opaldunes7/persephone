//! `persephone-cli plugin`: scaffold, build and install plugins.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use clap::{Subcommand, ValueEnum};
use persephone_core::indicator::Param;
use persephone_plugin_host::{Host, Plugin};

#[derive(Subcommand)]
pub enum PluginCommand {
    /// Create a new plugin crate, ready to build.
    New {
        /// Crate name, e.g. `hull-ma`.
        name: String,
        #[arg(long, value_enum, default_value_t = Kind::Indicator)]
        kind: Kind,
        /// Folder to create it in.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Build a plugin crate for wasm32-wasip2 and install it into the plugins folder, where
    /// a running Persephone picks it up.
    Build {
        /// The plugin crate.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Print the plugins folder.
    Dir,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    Indicator,
    Strategy,
}

pub fn run(command: &PluginCommand) -> anyhow::Result<()> {
    match command {
        PluginCommand::New { name, kind, dir } => new(name, *kind, dir),
        PluginCommand::Build { path } => build(path),
        PluginCommand::Dir => {
            println!("{}", plugins_dir()?.display());
            Ok(())
        }
    }
}

fn plugins_dir() -> anyhow::Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no data folder on this system")?
        .join("persephone")
        .join("plugins");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn new(name: &str, kind: Kind, dir: &Path) -> anyhow::Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("plugin names use letters, digits, `-` and `_`");
    }
    let root = dir.join(name);
    if root.exists() {
        bail!("{} already exists", root.display());
    }
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join(".cargo"))?;

    let feature = match kind {
        Kind::Indicator => "indicator",
        Kind::Strategy => "strategy",
    };
    fs::write(root.join("Cargo.toml"), manifest(name, feature))?;
    fs::write(
        root.join(".cargo/config.toml"),
        "[build]\ntarget = \"wasm32-wasip2\"\n",
    )?;
    // rustup installs the WebAssembly target the first time this crate builds.
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"stable\"\ntargets = [\"wasm32-wasip2\"]\n",
    )?;
    fs::write(root.join(".gitignore"), "/target\n")?;
    fs::write(
        root.join("src/lib.rs"),
        match kind {
            Kind::Indicator => INDICATOR,
            Kind::Strategy => STRATEGY,
        }
        .replace("TYPE", &type_name(name))
        .replace("NAME", &title(name)),
    )?;

    println!("Created {}", root.display());
    println!(
        "Build and install it with: persephone-cli plugin build {}",
        root.display()
    );
    Ok(())
}

/// The SDK as a path dependency when this CLI was built from a Persephone checkout, otherwise
/// from GitHub.
fn manifest(name: &str, feature: &str) -> String {
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sdk");
    let sdk = match local.canonicalize() {
        Ok(path) => format!("path = {:?}", path.display().to_string()),
        Err(_) => "git = \"https://github.com/opaldunes7/persephone\"".to_string(),
    };
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
persephone-sdk = {{ {sdk}, features = ["{feature}"] }}

[profile.release]
lto = true
strip = true

# A workspace of its own, so it builds wherever it lives.
[workspace]
"#
    )
}

fn title(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The plugin's Rust type name: `trend-follower` → `TrendFollower`.
fn type_name(name: &str) -> String {
    title(name).replace(' ', "")
}

fn build(path: &Path) -> anyhow::Result<()> {
    let manifest = path.join("Cargo.toml");
    let text =
        fs::read_to_string(&manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let name = package_name(&text).context("Cargo.toml has no [package] name")?;

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .current_dir(path)
        .status()
        .context("running cargo")?;
    if !status.success() {
        bail!("the build failed");
    }

    let file = format!("{}.wasm", name.replace('-', "_"));
    let built = target_dir(path).join("wasm32-wasip2/release").join(&file);
    // Load it the way the app will, so a broken plugin fails here instead of replacing a
    // working one in the plugins folder.
    let host = Host::new().map_err(|e| anyhow!(e))?;
    let what = match host
        .load(&built)
        .map_err(|e| anyhow!("{}: {e}", built.display()))?
    {
        Plugin::Indicator(p) => format!(
            "indicator \"{}\" ({})",
            p.info.name,
            param_names(&p.info.params)
        ),
        Plugin::Strategy(p) => format!(
            "strategy \"{}\" ({})",
            p.info.name,
            param_names(&p.info.params)
        ),
    };
    let installed = plugins_dir()?.join(&file);
    fs::copy(&built, &installed).with_context(|| format!("copying {}", built.display()))?;
    println!("Installed {what} as {}", installed.display());
    Ok(())
}

fn param_names(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| p.name.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where cargo puts this crate's build output.
fn target_dir(path: &Path) -> PathBuf {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(path)
        .output();
    output
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|json| {
            let key = "\"target_directory\":\"";
            let start = json.find(key)? + key.len();
            let end = json[start..].find('"')? + start;
            Some(PathBuf::from(&json[start..end]))
        })
        .unwrap_or_else(|| path.join("target"))
}

/// `name = "..."` from a manifest's `[package]` table.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(value) = line.strip_prefix("name") {
            let value = value.trim_start().strip_prefix('=')?.trim();
            return Some(value.trim_matches('"').to_owned());
        }
    }
    None
}

const INDICATOR: &str = r#"//! NAME: a Persephone indicator plugin.

use persephone_sdk::{Bar, Indicator, IndicatorInfo, Param, export_indicator, ta};

struct TYPE;

impl Indicator for TYPE {
    fn describe() -> IndicatorInfo {
        // `overlay` draws on the prices; `pane` gives it a pane of its own.
        IndicatorInfo::overlay("NAME", "NAME")
            .param(Param::period("Length", 20))
            .line("Value")
    }

    fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let length = params[0] as usize;
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        // One series per output, as long as `bars`; NaN where there's no value yet.
        Ok(vec![ta::ema(&closes, length)])
    }
}

export_indicator!(TYPE);
"#;

const STRATEGY: &str = r#"//! NAME: a Persephone strategy plugin.

use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, export_strategy, ta::Ema};

struct TYPE {
    average: Ema,
}

impl Strategy for TYPE {
    fn describe() -> StrategyInfo {
        // Persephone adds a "Size %" parameter of its own.
        StrategyInfo::new("NAME", "Long above the average, short below it")
            .param(Param::period("Length", 50))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        Ok(Self { average: Ema::new(params[0] as usize) })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        self.average.update(bar.close);
        let average = self.average.value()?;
        // 1 long, -1 short, 0 flat, None keeps the current position.
        Some(if bar.close > average { 1 } else { -1 })
    }
}

export_strategy!(TYPE);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_package_name() {
        let manifest = "[package]\nname = \"hull-ma\"\nversion = \"0.1.0\"\n\n[dependencies]\nname = \"not this\"\n";
        assert_eq!(package_name(manifest).as_deref(), Some("hull-ma"));
        assert_eq!(package_name("[workspace]\n"), None);
    }

    #[test]
    fn titles_crate_names() {
        assert_eq!(title("hull-ma"), "Hull Ma");
        assert_eq!(title("my_cool_plugin"), "My Cool Plugin");
        assert_eq!(type_name("trend-follower"), "TrendFollower");
    }
}

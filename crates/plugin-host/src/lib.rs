//! Loads and runs Persephone's WebAssembly plugins.
//!
//! A plugin is a component exporting the `indicator` or `strategy` world from
//! `wit/persephone.wit`. Every call runs in a fresh sandbox: no files, no network, at most
//! [`MEMORY_LIMIT`] of memory, and a budget of executed instructions ("fuel") so a stuck
//! plugin is stopped instead of freezing the app. The clock is stopped and randomness is
//! seeded, so the same inputs always give the same results. A crash comes back as an error
//! carrying the plugin's own panic message.
//!
//! This crate knows nothing about Nautilus: strategies are handed to the engine as a
//! [`SignalFactory`].

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use persephone_core::{
    Candle,
    indicator::{IndicatorInfo, Output, OutputStyle, Param, Placement},
    signal::{Signal, SignalFactory},
    strategy::StrategyInfo,
};
use rand::{SeedableRng, rngs::StdRng};
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{
    HostMonotonicClock, HostWallClock, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
    p2::pipe::MemoryOutputPipe,
};

mod indicator_world {
    wasmtime::component::bindgen!({ world: "indicator", path: "../../wit" });
}

mod strategy_world {
    // Both worlds share `types`; reuse the indicator world's copy.
    wasmtime::component::bindgen!({
        world: "strategy",
        path: "../../wit",
        with: { "persephone:plugin/types": crate::indicator_world::persephone::plugin::types },
    });
}

use indicator_world::persephone::plugin::types as wit;

/// Most memory a plugin instance may use.
pub const MEMORY_LIMIT: usize = 256 << 20;
/// Instruction budget for describing a plugin.
const DESCRIBE_FUEL: u64 = 100_000_000;
/// Instruction budget for computing an indicator over all bars (about a second of work).
const COMPUTE_FUEL: u64 = 1_000_000_000;
/// Instruction budget for starting a strategy.
const START_FUEL: u64 = 100_000_000;
/// Instruction budget for one strategy call per bar.
const BAR_FUEL: u64 = 50_000_000;
/// How much of a plugin's stderr (its panic message) to keep.
const STDERR_CAPACITY: usize = 16 << 10;

/// The plugin runtime: one compiler and a cache of loaded plugins. Cheap to share.
pub struct Host {
    engine: Engine,
    linker: Linker<State>,
    /// Plugins already loaded, by path, with the file's modification time when loaded.
    cache: Mutex<HashMap<PathBuf, (SystemTime, Plugin)>>,
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host").finish_non_exhaustive()
    }
}

/// A loaded plugin.
#[derive(Clone, Debug)]
pub enum Plugin {
    Indicator(Arc<IndicatorPlugin>),
    Strategy(Arc<StrategyPlugin>),
}

/// Every plugin in a folder, by id (file name without `.wasm`), plus files that failed.
#[derive(Clone, Debug, Default)]
pub struct Plugins {
    pub indicators: BTreeMap<String, Arc<IndicatorPlugin>>,
    pub strategies: BTreeMap<String, Arc<StrategyPlugin>>,
    /// (file name, why it didn't load)
    pub errors: Vec<(String, String)>,
}

impl Host {
    /// # Errors
    ///
    /// Returns a message if the WebAssembly runtime can't start.
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.wasm_component_model(true).consume_fuel(true);
        let engine =
            Engine::new(&config).map_err(|e| format!("can't start the plugin runtime: {e}"))?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| e.to_string())?;
        Ok(Self {
            engine,
            linker,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Loads every `.wasm` file in `dir`. Unchanged files come from the cache.
    #[must_use]
    pub fn load_dir(&self, dir: &Path) -> Plugins {
        let mut plugins = Plugins::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return plugins;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "wasm"))
            .collect();
        paths.sort();

        for path in paths {
            let file = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match self.load(&path) {
                Ok(Plugin::Indicator(p)) => {
                    plugins.indicators.insert(p.id.clone(), p);
                }
                Ok(Plugin::Strategy(p)) => {
                    plugins.strategies.insert(p.id.clone(), p);
                }
                Err(error) => plugins.errors.push((file, error)),
            }
        }
        plugins
    }

    /// Loads one plugin file (compiling it unless it's cached and unchanged).
    ///
    /// # Errors
    ///
    /// Returns a message if the file isn't a valid Persephone plugin.
    pub fn load(&self, path: &Path) -> Result<Plugin, String> {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map_err(|e| format!("can't read {}: {e}", path.display()))?;
        if let Some((when, plugin)) = self.cache.lock().expect("cache lock").get(path)
            && *when == modified
        {
            return Ok(plugin.clone());
        }

        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} has no file name", path.display()))?;
        let component = Component::from_file(&self.engine, path)
            .map_err(|e| format!("not a WebAssembly component: {e:#}"))?;
        let plugin = self.inspect(id, &component)?;
        self.cache
            .lock()
            .expect("cache lock")
            .insert(path.to_owned(), (modified, plugin.clone()));
        Ok(plugin)
    }

    /// Works out which world a component exports and asks it to describe itself.
    fn inspect(&self, id: String, component: &Component) -> Result<Plugin, String> {
        let pre = self
            .linker
            .instantiate_pre(component)
            .map_err(|e| format!("imports something plugins can't have: {e:#}"))?;

        if let Ok(indicator) = indicator_world::IndicatorPre::new(pre.clone()) {
            let mut store = sandbox(&self.engine, DESCRIBE_FUEL)?;
            let bindings = indicator
                .instantiate(&mut store)
                .map_err(|e| failure(&store, &e))?;
            let info = bindings
                .call_describe(&mut store)
                .map_err(|e| failure(&store, &e))?;
            let info = indicator_info(info)?;
            return Ok(Plugin::Indicator(Arc::new(IndicatorPlugin {
                id,
                info: Arc::new(info),
                engine: self.engine.clone(),
                pre: indicator,
            })));
        }
        if let Ok(strategy) = strategy_world::StrategyPre::new(pre) {
            let mut store = sandbox(&self.engine, DESCRIBE_FUEL)?;
            let bindings = strategy
                .instantiate(&mut store)
                .map_err(|e| failure(&store, &e))?;
            let info = bindings
                .call_describe(&mut store)
                .map_err(|e| failure(&store, &e))?;
            let params = info
                .params
                .into_iter()
                .map(param)
                .collect::<Result<_, _>>()?;
            return Ok(Plugin::Strategy(Arc::new(StrategyPlugin {
                id,
                info: Arc::new(StrategyInfo::new(info.name, info.description, params)),
                engine: self.engine.clone(),
                pre: strategy,
            })));
        }
        Err("exports neither the indicator nor the strategy world".into())
    }
}

/// An indicator plugin, ready to compute.
pub struct IndicatorPlugin {
    pub id: String,
    pub info: Arc<IndicatorInfo>,
    engine: Engine,
    pre: indicator_world::IndicatorPre<State>,
}

impl fmt::Debug for IndicatorPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndicatorPlugin")
            .field("id", &self.id)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl IndicatorPlugin {
    /// One series per output, each as long as `candles`.
    ///
    /// # Errors
    ///
    /// Returns the plugin's own error, its panic message, or why the sandbox stopped it.
    pub fn compute(&self, params: &[f64], candles: &[Candle]) -> Result<Vec<Vec<f64>>, String> {
        let mut store = sandbox(&self.engine, COMPUTE_FUEL)?;
        let bindings = self
            .pre
            .instantiate(&mut store)
            .map_err(|e| failure(&store, &e))?;
        let bars: Vec<wit::Bar> = candles.iter().map(bar).collect();
        let outputs = bindings
            .call_compute(&mut store, params, &bars)
            .map_err(|e| failure(&store, &e))??;

        if outputs.len() != self.info.outputs.len() {
            return Err(format!(
                "returned {} series but describes {} outputs",
                outputs.len(),
                self.info.outputs.len()
            ));
        }
        if let Some(series) = outputs.iter().find(|s| s.len() != candles.len()) {
            return Err(format!(
                "returned {} values for {} bars",
                series.len(),
                candles.len()
            ));
        }
        Ok(outputs)
    }
}

/// A strategy plugin; each backtest gets its own instance through [`SignalFactory`].
pub struct StrategyPlugin {
    pub id: String,
    /// Includes the "Size %" parameter the host adds.
    pub info: Arc<StrategyInfo>,
    engine: Engine,
    pre: strategy_world::StrategyPre<State>,
}

impl fmt::Debug for StrategyPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StrategyPlugin")
            .field("id", &self.id)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl SignalFactory for StrategyPlugin {
    fn create(&self, params: &[f64]) -> Result<Box<dyn Signal>, String> {
        let mut store = sandbox(&self.engine, START_FUEL)?;
        let bindings = self
            .pre
            .instantiate(&mut store)
            .map_err(|e| failure(&store, &e))?;
        bindings
            .call_start(&mut store, params)
            .map_err(|e| failure(&store, &e))??;
        Ok(Box::new(StrategyInstance { store, bindings }))
    }
}

/// One running strategy plugin.
struct StrategyInstance {
    store: Store<State>,
    bindings: strategy_world::Strategy,
}

impl Signal for StrategyInstance {
    fn on_bar(&mut self, candle: &Candle) -> Result<Option<i8>, String> {
        self.store.set_fuel(BAR_FUEL).map_err(|e| e.to_string())?;
        let target = self
            .bindings
            .call_on_bar(&mut self.store, bar(candle))
            .map_err(|e| failure(&self.store, &e))?;
        match target {
            None | Some(-1..=1) => Ok(target),
            Some(other) => Err(format!("asked for position {other}; use 1, -1, 0 or none")),
        }
    }
}

/// What a sandboxed instance can reach: an empty WASI (no files, network or environment) that
/// collects stderr, plus its limits.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    stderr: MemoryOutputPipe,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// The seed behind a plugin's randomness, the same every run.
const SEED: u64 = 0x5045_5253_4550_484f;

/// The clock plugins see: stopped at the Unix epoch, so nothing depends on when a backtest
/// runs. Sleeping still waits in real time.
struct Frozen;

impl HostWallClock for Frozen {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }

    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

impl HostMonotonicClock for Frozen {
    fn resolution(&self) -> u64 {
        1
    }

    fn now(&self) -> u64 {
        0
    }
}

fn sandbox(engine: &Engine, fuel: u64) -> Result<Store<State>, String> {
    let stderr = MemoryOutputPipe::new(STDERR_CAPACITY);
    // Same inputs, same outputs: a frozen clock and seeded randomness keep backtests
    // repeatable.
    let wasi = WasiCtxBuilder::new()
        .stderr(stderr.clone())
        .wall_clock(Frozen)
        .monotonic_clock(Frozen)
        .secure_random(StdRng::seed_from_u64(SEED))
        .insecure_random(StdRng::seed_from_u64(SEED))
        .insecure_random_seed(u128::from(SEED))
        .build();
    let state = State {
        wasi,
        table: ResourceTable::new(),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        stderr,
    };
    let mut store = Store::new(engine, state);
    store.limiter(|state| &mut state.limits);
    store.set_fuel(fuel).map_err(|e| e.to_string())?;
    Ok(store)
}

/// Turns a trap into a message: the plugin's panic text if it printed one, otherwise what the
/// sandbox stopped.
fn failure(store: &Store<State>, error: &wasmtime::Error) -> String {
    let stderr = store.data().stderr.contents();
    let printed = String::from_utf8_lossy(&stderr);
    if let Some(message) = panic_message(&printed) {
        return message;
    }
    if printed.contains("memory allocation") {
        return format!("ran out of memory (plugins get {} MB)", MEMORY_LIMIT >> 20);
    }
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => "took too long and was stopped".into(),
        Some(trap) => format!("crashed: {trap}"),
        None => format!("failed: {error:#}"),
    }
}

/// `panicked: <message> (<file:line>)` from Rust's panic output, which puts the location on
/// the `panicked at` line and the message on the lines after it.
fn panic_message(stderr: &str) -> Option<String> {
    let mut lines = stderr.lines();
    let header = lines.find(|l| l.contains("panicked at"))?;
    let location = header
        .split_once("panicked at ")
        .map(|(_, at)| at.trim_end_matches(':').trim())
        .unwrap_or_default();
    let message: Vec<&str> = lines
        .take_while(|l| !l.starts_with("note:") && !l.trim().is_empty())
        .collect();
    Some(if message.is_empty() {
        format!("panicked at {location}")
    } else {
        format!("panicked: {} ({location})", message.join(" "))
    })
}

fn bar(c: &Candle) -> wit::Bar {
    wit::Bar {
        ts: c.ts,
        open: c.open,
        high: c.high,
        low: c.low,
        close: c.close,
        volume: c.volume,
    }
}

/// Checks a plugin's parameter description.
fn param(p: wit::Param) -> Result<Param, String> {
    let param = Param {
        name: p.name.into(),
        default: p.default,
        min: p.min,
        max: p.max,
        integer: p.integer,
    };
    if param.check(param.default).is_err() {
        return Err(format!(
            "parameter `{}` has default {} outside {}..={}",
            param.name, param.default, param.min, param.max
        ));
    }
    Ok(param)
}

/// Checks a plugin's indicator description and converts it.
fn indicator_info(info: wit::IndicatorInfo) -> Result<IndicatorInfo, String> {
    if info.outputs.is_empty() {
        return Err("describes no outputs".into());
    }
    let outputs: Vec<Output> = info
        .outputs
        .into_iter()
        .map(|o| Output {
            name: o.name.into(),
            style: match o.style {
                wit::OutputStyle::Line => OutputStyle::Line,
                wit::OutputStyle::Histogram => OutputStyle::Histogram,
            },
        })
        .collect();
    let fill = match info.fill {
        Some((a, b)) if (a as usize) < outputs.len() && (b as usize) < outputs.len() => {
            Some((a as usize, b as usize))
        }
        Some(_) => return Err("fills between outputs that don't exist".into()),
        None => None,
    };
    Ok(IndicatorInfo {
        name: info.name,
        short_name: info.short_name,
        placement: match info.placement {
            wit::Placement::Overlay => Placement::Overlay,
            wit::Placement::Pane => Placement::Pane,
        },
        params: info
            .params
            .into_iter()
            .map(param)
            .collect::<Result<_, _>>()?,
        outputs,
        fill,
        levels: info.levels,
        range: info.range,
    })
}

#[cfg(test)]
mod tests {
    use super::panic_message;

    #[test]
    fn panic_messages_are_read_from_stderr() {
        let stderr = "thread '<unnamed>' (1) panicked at src/lib.rs:13:9:\nboom after 10 bars\nnote: run with `RUST_BACKTRACE=1`\n";
        assert_eq!(
            panic_message(stderr).unwrap(),
            "panicked: boom after 10 bars (src/lib.rs:13:9)"
        );
        assert_eq!(panic_message("nothing to see"), None);
    }
}

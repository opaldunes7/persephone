//! The Build tab: the chart's strategy written as a logic tree, a rule at a time or as JSON. A tree
//! that can run is the chart's strategy at once, saved with the layout; its variables are the
//! strategy's parameters, which Optimize and the rest vary; and it can be saved by name for every
//! chart, portfolio and agent to use.

use persephone_research::{
    logic::Logic,
    logic_draft::{Draft, EXAMPLES},
    results::{SavedResult, save_result},
};

use super::*;
use crate::backtest::{BuildMessage, BuildSlot};

/// The note a strategy saved from the Build tab carries, by which the list names it.
pub(crate) const SAVED_NOTE: &str = "Built in the app.";

impl Persephone {
    /// A restored tile's strategy, when it's the tree saved with it: the tree is made ready to run
    /// first, and the values it was set to are kept.
    pub(crate) fn restore_draft(&mut self, draft: &Draft, saved: StrategySpec) -> StrategySpec {
        let Ok(logic) = draft.logic() else {
            return saved;
        };
        if saved.kind != StrategyKind::Plugin(logic.id()) {
            return saved;
        }
        let mut spec = self.register_draft(logic);
        if saved.params.len() == spec.params.len() {
            spec.params = saved.params;
        }
        spec
    }

    /// Makes `logic` one the backtests can run, and its strategy as a tree's are set: its
    /// variables at their starting values.
    fn register_draft(&mut self, logic: Logic) -> StrategySpec {
        let spec = logic.spec();
        self.draft_logic.insert(logic.id(), Arc::new(logic));
        spec
    }

    /// The tree the panel's chart has, or would have: its own, else the saved tree it's running,
    /// else one to start from.
    pub(crate) fn draft_of(&self, id: TileId) -> Draft {
        let Some(tile) = self.tiles.get(&id) else {
            return Draft::default();
        };
        if let Some(draft) = &tile.draft {
            return draft.clone();
        }
        tile.strategy
            .as_ref()
            .and_then(|strategy| match &strategy.spec.kind {
                StrategyKind::Plugin(kind) => self.lab_logic.get(kind),
                StrategyKind::BuiltIn(_) => None,
            })
            .and_then(|logic| Draft::from_json(logic.source()).ok())
            .unwrap_or_default()
    }

    /// Whether the chart's strategy is its own tree, as built.
    pub(crate) fn draft_on_chart(&self, id: TileId) -> bool {
        let Some(tile) = self.tiles.get(&id) else {
            return false;
        };
        match (&tile.draft, tile.strategy.as_ref().map(|s| &s.spec.kind)) {
            (Some(draft), Some(StrategyKind::Plugin(kind))) => {
                draft.logic().is_ok_and(|logic| logic.id() == *kind)
            }
            _ => false,
        }
    }

    /// Keeps `draft` as the chart's tree and, if it can run, makes it the chart's strategy, keeping
    /// the position size. If it can't, the last one that could keeps running and the panel says
    /// why.
    fn use_draft(&mut self, id: TileId, draft: Draft) -> Task<Message> {
        let logic = draft.logic();
        if let Some(tile) = self.tiles.get_mut(&id) {
            tile.draft = Some(draft);
        }
        let logic = match logic {
            Ok(logic) => logic,
            Err(error) => {
                if let Some(panel) = self.panel.as_mut() {
                    panel.build.error = Some(error);
                }
                self.save();
                return Task::none();
            }
        };
        if let Some(panel) = self.panel.as_mut() {
            panel.build.error = None;
        }
        let mut spec = self.register_draft(logic);
        // The size it was trading at stays.
        if let Some(size) = self
            .tiles
            .get(&id)
            .and_then(|tile| tile.strategy.as_ref())
            .and_then(|strategy| strategy.spec.params.last().copied())
            && let Some(last) = spec.params.last_mut()
        {
            *last = size;
        }
        let unchanged = self
            .tiles
            .get(&id)
            .and_then(|tile| tile.strategy.as_ref())
            .is_some_and(|strategy| strategy.spec == spec);
        if unchanged {
            self.save();
            return Task::none();
        }
        self.set_strategy(id, spec)
    }

    /// Optimize's values put on a tree are its variables' new starting values, so editing it
    /// afterwards keeps them.
    pub(crate) fn keep_values_in_draft(&mut self, id: TileId) {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return;
        };
        let (Some(draft), Some(strategy)) = (tile.draft.as_mut(), tile.strategy.as_ref()) else {
            return;
        };
        let StrategyKind::Plugin(kind) = &strategy.spec.kind else {
            return;
        };
        if !draft.logic().is_ok_and(|logic| logic.id() == *kind) {
            return;
        }
        for (param, value) in strategy.spec.info.params.iter().zip(&strategy.spec.params) {
            if let Some(start) = draft.params.get_mut(param.name.as_ref()) {
                *start = *value;
            }
        }
        // The tree's own values changed, so it's a new tree: it runs as before under its new id.
        if let Ok(logic) = draft.logic() {
            let spec = logic.spec();
            let params = strategy.spec.params.clone();
            self.draft_logic.insert(logic.id(), Arc::new(logic));
            if let Some(strategy) = tile.strategy.as_mut() {
                strategy.spec = StrategySpec { params, ..spec };
            }
        }
    }

    fn build_state(&mut self) -> Option<&mut backtest::BuildState> {
        self.panel.as_mut().map(|panel| &mut panel.build)
    }

    pub(crate) fn on_build(&mut self, message: BuildMessage) -> Task<Message> {
        let Some(id) = self.panel.as_ref().map(|panel| panel.tile) else {
            return Task::none();
        };
        let mut draft = self.draft_of(id);
        // Boxes being typed into keep their text only while their place in the tree stays put.
        let typing = matches!(
            message,
            BuildMessage::Typed(..) | BuildMessage::ValueTyped(..) | BuildMessage::RenameTyped(..)
        );
        if !typing && let Some(build) = self.build_state() {
            build.typed.clear();
            build.values.clear();
        }
        let changed = match message {
            BuildMessage::StartFrom(example) => {
                draft = match example.and_then(|at| EXAMPLES.get(at)) {
                    Some((_, text)) => serde_json::from_str(text)
                        .ok()
                        .and_then(|json| Draft::from_json(&json).ok())
                        .unwrap_or_default(),
                    None => Draft::default(),
                };
                true
            }
            BuildMessage::UseOnChart => true,
            BuildMessage::Name(name) => {
                draft.name = name;
                true
            }
            BuildMessage::Side(wish, on) => {
                draft.set_side(wish, on);
                true
            }
            BuildMessage::Add(path, group) => draft.add(&path, group),
            BuildMessage::Remove(path) => draft.remove(&path),
            BuildMessage::Group(path, all) => draft.set_group(&path, all),
            BuildMessage::Not(path) => draft.toggle_not(&path),
            BuildMessage::Comparison(path, op) => draft.set_comparison(&path, op),
            BuildMessage::Kind(path, kind) => draft.set_kind(&path, kind),
            BuildMessage::Timeframe(path, timeframe) => draft.set_timeframe(&path, &timeframe),
            BuildMessage::Typed(path, slot, text) => {
                let applied = match slot {
                    BuildSlot::Number => text
                        .trim()
                        .parse::<f64>()
                        .is_ok_and(|value| draft.set_number(&path, value)),
                    BuildSlot::Bars => text
                        .trim()
                        .parse::<u32>()
                        .is_ok_and(|bars| draft.set_amount(&path, bars)),
                };
                if let Some(build) = self.build_state() {
                    build.set_typed(path, slot, text);
                }
                applied
            }
            BuildMessage::Variable(path, slot) => match slot {
                BuildSlot::Bars => draft.toggle_amount_variable(&path),
                BuildSlot::Number => draft.to_variable(&path) || draft.to_constant(&path),
            },
            BuildMessage::RenameTyped(name, text) => {
                if let Some(build) = self.build_state() {
                    build.renaming.retain(|(known, _)| *known != name);
                    build.renaming.push((name, text));
                }
                false
            }
            BuildMessage::Rename(name) => {
                let to = self.panel.as_ref().and_then(|panel| {
                    panel
                        .build
                        .renaming
                        .iter()
                        .find(|(known, _)| *known == name)
                        .map(|(_, text)| text.clone())
                });
                match to.map(|to| draft.rename(&name, &to)) {
                    Some(Ok(())) => {
                        if let Some(build) = self.build_state() {
                            build.renaming.retain(|(known, _)| *known != name);
                        }
                        true
                    }
                    Some(Err(error)) => {
                        if let Some(build) = self.build_state() {
                            build.error = Some(error);
                        }
                        false
                    }
                    None => false,
                }
            }
            BuildMessage::ValueTyped(name, text) => {
                let value = text.trim().parse::<f64>().ok().filter(|v| v.is_finite());
                if let Some(build) = self.build_state() {
                    build.values.retain(|(known, _)| *known != name);
                    build.values.push((name.clone(), text));
                }
                match (value, draft.params.get_mut(&name)) {
                    (Some(value), Some(start)) => {
                        *start = value;
                        true
                    }
                    _ => false,
                }
            }
            BuildMessage::JsonMode(on) => {
                if let Some(build) = self.build_state() {
                    build.json_mode = on;
                    build.error = None;
                    if on {
                        let text =
                            serde_json::to_string_pretty(&draft.to_json()).unwrap_or_default();
                        build.json = iced::widget::text_editor::Content::with_text(&text);
                    }
                }
                return Task::none();
            }
            BuildMessage::JsonEdit(action) => {
                if let Some(build) = self.build_state() {
                    build.json.perform(action);
                }
                return Task::none();
            }
            BuildMessage::ApplyJson => {
                let text = self
                    .panel
                    .as_ref()
                    .map(|panel| panel.build.json.text())
                    .unwrap_or_default();
                let read = serde_json::from_str::<serde_json::Value>(&text)
                    .map_err(|e| format!("That isn't JSON: {e}"))
                    .and_then(|json| Draft::from_json(&json));
                match read {
                    Ok(read) => {
                        draft = read;
                        if let Some(build) = self.build_state() {
                            build.json_mode = false;
                        }
                        true
                    }
                    Err(error) => {
                        if let Some(build) = self.build_state() {
                            build.error = Some(error);
                        }
                        return Task::none();
                    }
                }
            }
            BuildMessage::CopyJson => {
                let text = serde_json::to_string_pretty(&draft.to_json()).unwrap_or_default();
                return iced::clipboard::write(text);
            }
            BuildMessage::SaveName(name) => {
                if let Some(build) = self.build_state() {
                    build.save_name = name;
                }
                return Task::none();
            }
            BuildMessage::Save => return self.save_draft(id, draft),
        };
        if !changed {
            return Task::none();
        }
        if let Some(build) = self.build_state() {
            build.saved = None;
        }
        self.use_draft(id, draft)
    }

    /// Saves the chart's tree by name, with its settings, where every chart, portfolio and agent
    /// finds saved strategies, and puts the saved one on the chart.
    fn save_draft(&mut self, id: TileId, mut draft: Draft) -> Task<Message> {
        let Some(dir) = self.results_dir.clone() else {
            if let Some(panel) = self.panel.as_mut() {
                panel.build.error =
                    Some("There's nowhere to save strategies on this system.".into());
            }
            return Task::none();
        };
        let name = self
            .panel
            .as_ref()
            .map(|panel| panel.build.save_name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| draft.name.clone());
        draft.name.clone_from(&name);
        if let Err(error) = draft.logic() {
            if let Some(panel) = self.panel.as_mut() {
                panel.build.error = Some(error);
            }
            return Task::none();
        }
        let size = self
            .tiles
            .get(&id)
            .and_then(|tile| tile.strategy.as_ref())
            .and_then(|strategy| strategy.spec.params.last().copied())
            .unwrap_or(100.0);
        let saved = SavedResult {
            id: format!("result-{}-app", now_ms()),
            name: name.clone(),
            note: SAVED_NOTE.into(),
            strategy: serde_json::json!({"logic": draft.to_json(), "params": {"Size %": size}}),
            settings: self.tiles.get(&id).map(|tile| tile.backtest.clone()),
            summary: serde_json::Value::Null,
            saved_at: now_ms(),
        };
        match save_result(&dir, &saved) {
            Ok(_) => {
                if let Some(panel) = self.panel.as_mut() {
                    panel.build.error = None;
                    panel.build.saved = Some(format!(
                        "Saved as {name}: it's in every chart's strategies and portfolio legs, and Lab's list_results."
                    ));
                }
                let task = self.use_draft(id, draft);
                // Read it straight away rather than waiting on the folder's watcher, so it's
                // listed now.
                Task::batch([task, self.load_lab_results()])
            }
            Err(error) => {
                if let Some(panel) = self.panel.as_mut() {
                    panel.build.error = Some(format!("Couldn't save it: {error}"));
                }
                Task::none()
            }
        }
    }

    /// The tree for the Build tab to show: the chart's, or where it would start.
    #[must_use]
    pub(crate) fn shown_draft(&self) -> Option<(Draft, bool)> {
        let id = self.panel.as_ref()?.tile;
        Some((self.draft_of(id), self.draft_on_chart(id)))
    }
}

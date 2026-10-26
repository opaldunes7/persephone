//! What plugins can reach outside their sandbox: the models folder (`ml`) for every plugin, and
//! language models (`ai`) for strategies the user has allowed.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, RwLock},
};

use persephone_core::{UnixNanos, signal::Note, time::DateTime};
use persephone_ml::{
    ai::{self, Ai},
    models::{self, Models},
};

use crate::{
    State, indicator_world::persephone::plugin::ml,
    strategy_world::persephone::plugin::ai as wit_ai,
};

/// The models folder and the language-model client plugins may use, and which plugins may use
/// the latter.
#[derive(Debug, Default)]
pub struct Services {
    pub models: Option<Arc<Models>>,
    pub ai: Option<Arc<Ai>>,
    /// Plugins allowed to call language models, by id.
    allowed: RwLock<HashSet<String>>,
}

impl Services {
    #[must_use]
    pub fn new(models: Option<Arc<Models>>, ai: Option<Arc<Ai>>) -> Self {
        Self {
            models,
            ai,
            allowed: RwLock::default(),
        }
    }

    /// Lets plugin `id` call language models, or stops it.
    pub fn allow_ai(&self, id: &str, allow: bool) {
        if let Ok(mut allowed) = self.allowed.write() {
            if allow {
                allowed.insert(id.to_owned());
            } else {
                allowed.remove(id);
            }
        }
    }

    #[must_use]
    pub fn ai_allowed(&self, id: &str) -> bool {
        self.allowed
            .read()
            .is_ok_and(|allowed| allowed.contains(id))
    }

    /// The client for plugin `id`, if it's allowed and there is one.
    pub(crate) fn ai_for(&self, id: &str) -> Option<Arc<Ai>> {
        self.ai.clone().filter(|_| self.ai_allowed(id))
    }
}

/// How one strategy instance used language models.
#[derive(Debug, Default)]
pub(crate) struct AiUsage {
    answered: u32,
    /// Answers that came from recordings.
    recorded: u32,
    failed: u32,
    first_failure: Option<String>,
    /// The earliest bar each model was asked about.
    models: BTreeMap<String, UnixNanos>,
    /// A question went unanswered because the backtest's new calls ran out.
    out_of_calls: bool,
    /// It asked without being allowed.
    denied: bool,
}

impl ml::Host for State {
    fn run(&mut self, name: String, input: ml::Tensor) -> Result<Vec<ml::Tensor>, String> {
        let models = self
            .models
            .as_ref()
            .ok_or("models aren't available (there's no models folder)")?;
        let input = models::Tensor {
            shape: input.shape.iter().map(|&d| d as usize).collect(),
            data: input.data,
        };
        let outputs = models.run(&name, &input)?;
        Ok(outputs
            .into_iter()
            .map(|t| ml::Tensor {
                shape: t
                    .shape
                    .iter()
                    .map(|&d| u32::try_from(d).unwrap_or(u32::MAX))
                    .collect(),
                data: t.data,
            })
            .collect())
    }
}

impl wit_ai::Host for State {
    fn complete(&mut self, request: wit_ai::Request) -> Result<String, String> {
        let Some(client) = self.ai.clone() else {
            if self.ai_allowed {
                let error = String::from("language models aren't set up (is ai.json valid?)");
                self.usage.failed += 1;
                self.usage
                    .first_failure
                    .get_or_insert_with(|| error.clone());
                return Err(error);
            }
            self.usage.denied = true;
            return Err(
                "this plugin isn't allowed to call language models; allow it in its strategy report"
                    .into(),
            );
        };
        let request = ai::Request {
            model: request.model,
            system: request.system,
            prompt: request.prompt,
            max_tokens: request.max_tokens,
            effort: request.effort.map(|e| match e {
                wit_ai::Effort::Low => ai::Effort::Low,
                wit_ai::Effort::Medium => ai::Effort::Medium,
                wit_ai::Effort::High => ai::Effort::High,
            }),
        };
        let new_calls = self.usage.answered - self.usage.recorded;
        let may_call = new_calls < self.budget;
        let usage = &mut self.usage;
        match client.complete(&request, may_call) {
            Ok(reply) => {
                usage.answered += 1;
                usage.recorded += u32::from(reply.cached);
                if let Some(bar) = self.bar {
                    let first = usage.models.entry(request.model).or_insert(bar);
                    *first = (*first).min(bar);
                }
                Ok(reply.text)
            }
            Err(error) => {
                usage.failed += 1;
                usage.out_of_calls |= !may_call;
                usage.first_failure.get_or_insert_with(|| error.clone());
                Err(error)
            }
        }
    }
}

/// What the report should say about a run's AI use: how much, what failed, and whether the
/// models could already have known what happened.
pub(crate) fn notes(usage: &AiUsage, ai: Option<&Ai>, budget: u32) -> Vec<Note> {
    let note = |text: String, warning| Note { text, warning };
    let date = |ts: UnixNanos| DateTime::from_nanos(ts).date();
    let mut notes = Vec::new();
    if usage.denied {
        notes.push(note(
            "Its questions to language models went unanswered: it isn't allowed to ask them yet."
                .into(),
            true,
        ));
    }
    if usage.answered > 0 {
        let models: Vec<&str> = usage.models.keys().map(String::as_str).collect();
        notes.push(note(
            format!(
                "Asked {} {} times: {} answered from recordings, {} new calls.",
                models.join(" and "),
                usage.answered,
                usage.recorded,
                usage.answered - usage.recorded
            ),
            false,
        ));
    }
    if usage.out_of_calls {
        notes.push(note(
            format!(
                "Hit the limit of {budget} new AI calls for one backtest, so later questions went unanswered; raise max_new_calls in ai.json to allow more."
            ),
            true,
        ));
    }
    if usage.failed > 0 {
        notes.push(note(
            format!(
                "{} questions got no answer (the first: {}).",
                usage.failed,
                usage.first_failure.as_deref().unwrap_or("unknown")
            ),
            true,
        ));
    }
    for (model, &first) in &usage.models {
        match ai.and_then(|ai| ai.release_date(model)) {
            Some(release) if first < release => notes.push(note(
                format!(
                    "Look-ahead: {model} came out on {}, after the bars it was asked about (from {}), so it may already know what happened next. Only bars after {} are a fair test of it.",
                    date(release),
                    date(first),
                    date(release)
                ),
                true,
            )),
            Some(_) => {}
            None => notes.push(note(
                format!(
                    "Look-ahead: {model} may have been trained on data covering these bars, so it may already know what happened next, and results can look better than it would really do."
                ),
                true,
            )),
        }
    }
    notes
}

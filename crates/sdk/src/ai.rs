//! Language models, for strategies.
//!
//! A strategy that calls [`complete`] can't reach a model until the user allows it in the app
//! (prompts leave the machine, and calls can cost money); until then `complete` returns an
//! error. Every reply is recorded, so a backtest asks each distinct question once and reruns
//! get the same answers. Ask sparingly (once a day rather than every bar): each backtest may
//! make only so many new calls.
//!
//! ```ignore
//! let reply = ai::complete(
//!     &ai::Request::new("claude-opus-5-5", prompt)
//!         .system("Reply with one word: LONG, SHORT or FLAT.")
//!         .effort(ai::Effort::Low),
//! )?;
//! ```

use crate::strategy_bindings::persephone::plugin::ai as host;

/// How hard models that support it should think.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// One question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// A Claude model id (`claude-opus-5-5`), `openai/<model>` or `ollama/<model>`.
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    /// Output allowance; models that think count their thinking against it.
    pub max_tokens: Option<u32>,
    pub effort: Option<Effort>,
}

impl Request {
    #[must_use]
    pub fn new(model: &str, prompt: impl Into<String>) -> Self {
        Self {
            model: model.to_owned(),
            system: None,
            prompt: prompt.into(),
            max_tokens: None,
            effort: None,
        }
    }

    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    #[must_use]
    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    #[must_use]
    pub fn effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }
}

/// The model's reply.
///
/// # Errors
///
/// Returns why there's no reply: not allowed yet, out of new calls for this backtest, no API
/// key, the model is unreachable or declined.
pub fn complete(request: &Request) -> Result<String, String> {
    host::complete(&host::Request {
        model: request.model.clone(),
        system: request.system.clone(),
        prompt: request.prompt.clone(),
        max_tokens: request.max_tokens,
        effort: request.effort.map(|e| match e {
            Effort::Low => host::Effort::Low,
            Effort::Medium => host::Effort::Medium,
            Effort::High => host::Effort::High,
        }),
    })
}

//! `persephone-cli models` and `persephone-cli ai`: the models folder plugins run ONNX models
//! from, and how they reach language models.

use std::fs;

use anyhow::{Context, bail};
use clap::Subcommand;
use persephone_ml::{
    ai::{self, Ai, Cache, Config},
    models::{Models, logistic_regression},
};

#[derive(Subcommand)]
pub enum ModelsCommand {
    /// Print the models folder.
    Dir,
    /// List the models in it.
    List,
    /// Write a starter `trend.onnx` for the ML Trend example strategy: a logistic regression
    /// that leans with the last four bar returns.
    Example {
        /// Replace an existing `trend.onnx`.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum AiCommand {
    /// Show what's set up: where keys come from (never the keys), offline mode, limits, and
    /// how many replies are recorded.
    Status,
    /// Forget every recorded reply. Later backtests ask the models again, and pay again.
    ClearRecordings {
        /// Really forget them.
        #[arg(long)]
        yes: bool,
    },
}

pub fn models(command: &ModelsCommand) -> anyhow::Result<()> {
    let dir = Models::default_dir().context("no data folder on this system")?;
    match command {
        ModelsCommand::Dir => println!("{}", dir.display()),
        ModelsCommand::List => {
            let names = Models::new(&dir).names();
            if names.is_empty() {
                println!(
                    "No models in {}. Save ONNX files there as <name>.onnx, or try `persephone-cli models example`.",
                    dir.display()
                );
            }
            for name in names {
                println!("{name}");
            }
        }
        ModelsCommand::Example { force } => {
            let path = dir.join("trend.onnx");
            if path.exists() && !force {
                bail!(
                    "{} already exists; add --force to replace it",
                    path.display()
                );
            }
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            // Weights on the four returns, oldest to newest: recent moves count most.
            fs::write(&path, logistic_regression(&[0.5, 1.0, 1.5, 2.0], 0.0))
                .with_context(|| format!("writing {}", path.display()))?;
            println!("Wrote {}", path.display());
            println!(
                "It's a toy momentum model; train a real one with plugins/examples/ml-trend/train.py."
            );
        }
    }
    Ok(())
}

pub fn ai(command: &AiCommand) -> anyhow::Result<()> {
    let (config_path, cache_dir) = ai::default_paths().context("no data folder on this system")?;
    match command {
        AiCommand::Status => {
            let from_file: Config = fs::read_to_string(&config_path)
                .ok()
                .map(|text| serde_json::from_str(&text))
                .transpose()
                .with_context(|| format!("{} isn't valid", config_path.display()))?
                .unwrap_or_default();
            let found = if config_path.exists() {
                "found"
            } else {
                "not there; using defaults"
            };
            println!("Config      {} ({found})", config_path.display());
            let source = |env: &str, file: &Option<String>| {
                if std::env::var(env).is_ok_and(|v| !v.is_empty()) {
                    format!("key from {env}")
                } else if file.is_some() {
                    "key from ai.json".to_owned()
                } else {
                    "no key".to_owned()
                }
            };
            let claude = if from_file.anthropic_auth_token.is_some()
                || std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok_and(|v| !v.is_empty())
            {
                "OAuth token".to_owned()
            } else {
                source("ANTHROPIC_API_KEY", &from_file.anthropic_api_key)
            };
            println!("Claude      {claude}");
            println!(
                "OpenAI      {}",
                source("OPENAI_API_KEY", &from_file.openai_api_key)
            );
            println!(
                "Ollama      {}",
                from_file
                    .ollama_base_url
                    .as_deref()
                    .unwrap_or("http://localhost:11434/v1")
            );
            let ai = Ai::new(Config::load(&config_path)?, &cache_dir)?;
            println!(
                "Offline     {}",
                if ai.config().offline {
                    "yes: recorded replies only"
                } else {
                    "no"
                }
            );
            println!("New calls   {} per backtest", ai.max_new_calls());
            println!(
                "Recorded    {} replies in {}",
                ai.cache().len(),
                cache_dir.display()
            );
        }
        AiCommand::ClearRecordings { yes } => {
            let cache = Cache::new(&cache_dir);
            if *yes {
                cache.clear()?;
                println!("Forgot every recorded reply.");
            } else {
                println!(
                    "This forgets {} recorded replies; backtests will ask (and pay) again. Run again with --yes to do it.",
                    cache.len()
                );
            }
        }
    }
    Ok(())
}

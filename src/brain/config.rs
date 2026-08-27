//! `brain/config.toml` — how the brain compiles, loads and flushes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Bumped when the on-disk brain layout changes in a way that needs an upgrade
/// step. An existing brain is never wiped — see [`super::init`].
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BrainConfig {
    #[serde(default)]
    pub brain: BrainSection,
    #[serde(default)]
    pub compiler: CompilerConfig,
    #[serde(default)]
    pub load: LoadConfig,
    #[serde(default)]
    pub flush: FlushConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BrainSection {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompilerConfig {
    /// `"claude-cli"` or `"gemini-api"`.
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Engine-specific model name (`haiku`/`sonnet`, or a Gemini model id).
    #[serde(default = "default_model")]
    pub model: String,
    /// Ceiling on how much input the compiler processes per day.
    #[serde(default = "default_daily_budget")]
    pub daily_token_budget: usize,
    /// Local `HH:MM` for the scheduled run; empty means manual only.
    #[serde(default = "default_schedule")]
    pub schedule: String,
    #[serde(default)]
    pub gemini: GeminiConfig,
}

/// Gemini settings. The API key is **never** stored here — it is read from
/// `GEMINI_API_KEY` at call time.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GeminiConfig {
    #[serde(default = "default_gemini_model")]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoadConfig {
    /// Budget for the session-opening `brain_load` package.
    #[serde(default = "default_load_tokens")]
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FlushConfig {
    /// Empty means "use `compiler.model`".
    #[serde(default)]
    pub model_override: String,
}

fn default_schema_version() -> u32 { SCHEMA_VERSION }
fn default_engine() -> String { "claude-cli".to_string() }
fn default_model() -> String { "haiku".to_string() }
fn default_daily_budget() -> usize { 200_000 }
fn default_schedule() -> String { "18:00".to_string() }
fn default_gemini_model() -> String { "gemini-2.5-flash".to_string() }
fn default_load_tokens() -> usize { 4_000 }

impl Default for BrainSection {
    fn default() -> Self { Self { schema_version: SCHEMA_VERSION } }
}
impl Default for CompilerConfig {
    fn default() -> Self {
        Self {
            engine: default_engine(),
            model: default_model(),
            daily_token_budget: default_daily_budget(),
            schedule: default_schedule(),
            gemini: GeminiConfig::default(),
        }
    }
}
impl Default for GeminiConfig {
    fn default() -> Self { Self { model: default_gemini_model() } }
}
impl Default for LoadConfig {
    fn default() -> Self { Self { max_tokens: default_load_tokens() } }
}

impl BrainConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read brain config: {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("Cannot parse brain config: {}", path.display()))
    }

    /// The model session flushes should use instead of the compiler's, if the
    /// user set one.
    ///
    /// Session summaries run after every session and are an easy task; the
    /// nightly compile runs once and is a hard one. Splitting them is the point
    /// of this setting — `compiler.model = "sonnet"` with
    /// `flush.model_override = "haiku"` is the usual shape.
    pub fn flush_model(&self) -> Option<&str> {
        let override_ = self.flush.model_override.trim();
        (!override_.is_empty()).then_some(override_)
    }

    /// The starting config file. Written once; the user owns it afterwards, so
    /// upgrades touch `schema_version` and nothing else.
    pub fn template(engine: &str, model: &str) -> String {
        format!(
            r#"[brain]
schema_version = {SCHEMA_VERSION}

[compiler]
engine = "{engine}"          # "claude-cli" | "gemini-api"
model  = "{model}"           # claude-cli: haiku|sonnet ; gemini-api: a Gemini model id
daily_token_budget = 200000  # most input the compiler will chew through per day
schedule = "18:00"           # local time; empty = manual only

  [compiler.gemini]
  # The API key is read from GEMINI_API_KEY — it is NEVER written to this file.
  model = "gemini-2.5-flash"

[load]
max_tokens = 4000            # budget for the brain_load opening package

[flush]
model_override = ""          # empty = use compiler.model
"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trips() {
        let text = BrainConfig::template("claude-cli", "haiku");
        let cfg: BrainConfig = toml::from_str(&text).unwrap();

        assert_eq!(cfg.brain.schema_version, SCHEMA_VERSION);
        assert_eq!(cfg.compiler.engine, "claude-cli");
        assert_eq!(cfg.compiler.model, "haiku");
        assert_eq!(cfg.compiler.daily_token_budget, 200_000);
        assert_eq!(cfg.load.max_tokens, 4_000);
        assert_eq!(cfg.compiler.gemini.model, "gemini-2.5-flash");
        // No API key anywhere in the file.
        assert!(!text.to_lowercase().contains("api_key ="));
    }

    #[test]
    fn missing_sections_fall_back_to_defaults() {
        let cfg: BrainConfig = toml::from_str("[brain]\nschema_version = 1\n").unwrap();
        assert_eq!(cfg.compiler.engine, "claude-cli");
        assert_eq!(cfg.load.max_tokens, 4_000);
    }

    #[test]
    fn flush_model_override_wins_when_set() {
        let mut cfg = BrainConfig::default();
        // Unset means "use the compiler's model", not a second copy of it.
        assert_eq!(cfg.flush_model(), None);

        cfg.flush.model_override = "sonnet".into();
        assert_eq!(cfg.flush_model(), Some("sonnet"));

        // Whitespace is not a setting.
        cfg.flush.model_override = "   ".into();
        assert_eq!(cfg.flush_model(), None);
    }
}

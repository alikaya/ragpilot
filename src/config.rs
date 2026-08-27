use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub project:      ProjectConfig,
    pub embedding:    EmbeddingConfig,
    pub qdrant:       QdrantConfig,
    pub indexing:     IndexingConfig,
    pub mcp:          McpConfig,
    #[serde(default)]
    pub watcher:      WatcherConfig,
    #[serde(default)]
    pub symbol_graph: SymbolGraphConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectConfig {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingConfig {
    pub provider: String,
    #[serde(default)]
    pub local: LocalEmbeddingConfig,
    #[serde(default)]
    pub api: ApiEmbeddingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalEmbeddingConfig {
    #[serde(default = "default_local_model")]
    pub model: String,
    pub cache_dir: Option<String>,
}

impl Default for LocalEmbeddingConfig {
    fn default() -> Self {
        Self { model: default_local_model(), cache_dir: None }
    }
}

fn default_local_model() -> String {
    "BAAI/bge-small-en-v1.5".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiEmbeddingConfig {
    #[serde(default = "default_api_provider")]
    pub provider: String,
    #[serde(default = "default_api_model")]
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

impl Default for ApiEmbeddingConfig {
    fn default() -> Self {
        Self {
            provider:    default_api_provider(),
            model:       default_api_model(),
            api_key_env: default_api_key_env(),
            batch_size:  default_batch_size(),
        }
    }
}

fn default_api_provider() -> String { "openai".to_string() }
fn default_api_model()    -> String { "text-embedding-3-small".to_string() }
fn default_api_key_env()  -> String { "OPENAI_API_KEY".to_string() }
fn default_batch_size()   -> usize  { 32 }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QdrantConfig {
    #[serde(default = "default_qdrant_url")]
    pub url: String,
    pub collection: Option<String>,
    #[serde(default)]
    pub vector_size: u64,
    pub api_key: Option<String>,
}

impl QdrantConfig {
    pub fn collection_name(&self, project_name: &str) -> String {
        let base = self.collection.as_deref().unwrap_or(project_name);
        base.to_lowercase().replace([' ', '-'], "_")
    }
}

fn default_qdrant_url() -> String {
    "http://localhost:6334".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexingConfig {
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    #[serde(default = "default_include_extensions")]
    pub include_extensions: Vec<String>,
    #[serde(default = "default_exclude_dirs")]
    pub exclude_dirs: Vec<String>,
    #[serde(default = "default_include_dirs")]
    pub include_dirs: Vec<String>,
    #[serde(default = "default_max_file_size")]
    pub max_file_size_kb: u64,
    #[serde(default = "default_max_parallel_files")]
    pub max_parallel_files: usize,
    #[serde(default = "default_embedding_batch_size")]
    pub embedding_batch_size: usize,
    #[serde(default = "default_max_parallel_embeddings")]
    pub max_parallel_embeddings: usize,
    #[serde(default = "bool_true")]
    pub skip_minified: bool,
    #[serde(default = "bool_true")]
    pub skip_binary: bool,
}

fn default_chunk_size() -> usize { 700 }
fn default_chunk_overlap() -> usize { 80 }
fn default_max_file_size() -> u64 { 250 }
fn default_max_parallel_files() -> usize { 2 }
fn default_embedding_batch_size() -> usize { 16 }
fn default_max_parallel_embeddings() -> usize { 1 }

fn default_include_extensions() -> Vec<String> {
    ["rs", "toml", "md"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_exclude_dirs() -> Vec<String> {
    [
        ".git", ".rag", "target", "node_modules", "__pycache__", ".venv", "venv", "dist", "build",
        ".next", ".nuxt", "vendor", "coverage", ".cache", ".turbo", ".idea", ".vscode",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_include_dirs() -> Vec<String> {
    ["src"].iter().map(|s| s.to_string()).collect()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpConfig {
    #[serde(default = "default_context_chunks")]
    pub context_chunks: usize,
    #[serde(default = "default_bundle_budget")]
    pub bundle_budget_tokens: usize,
    #[serde(default = "default_search_tool_description")]
    pub search_tool_description: String,
    #[serde(default = "default_max_context_files")]
    pub max_context_files: usize,
    #[serde(default = "default_max_context_chunks")]
    pub max_context_chunks: usize,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    #[serde(default)]
    pub auto_update_before_search: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            context_chunks: default_context_chunks(),
            bundle_budget_tokens: default_bundle_budget(),
            search_tool_description: default_search_tool_description(),
            max_context_files: default_max_context_files(),
            max_context_chunks: default_max_context_chunks(),
            max_context_tokens: default_max_context_tokens(),
            auto_update_before_search: false,
        }
    }
}

fn default_bundle_budget() -> usize { 6000 }
fn default_max_context_files() -> usize { 8 }
fn default_max_context_chunks() -> usize { 20 }
fn default_max_context_tokens() -> usize { 12000 }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatcherConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default)]
    pub git_hook: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self { enabled: false, debounce_ms: default_debounce_ms(), git_hook: false }
    }
}

fn default_debounce_ms() -> u64 { 2000 }
fn bool_true() -> bool { true }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SymbolGraphConfig {
    #[serde(default = "bool_true")]
    pub enabled: bool,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
}

impl Default for SymbolGraphConfig {
    fn default() -> Self {
        Self { enabled: true, max_depth: default_max_depth(), max_nodes: default_max_nodes() }
    }
}

fn default_max_depth() -> usize { 2 }
fn default_max_nodes() -> usize { 200 }

fn default_context_chunks() -> usize { 4 }

fn default_search_tool_description() -> String {
    "Searches the local project codebase and documentation using semantic similarity. \
     Call this tool whenever the user asks about: how code works, where something is \
     implemented, project structure, functions, modules, configuration, or any \
     project-specific question. Returns relevant code snippets and docs with file paths."
        .to_string()
}

/// Render a list of strings as a multi-line TOML array literal.
/// Empty input renders `[]` on a single line.
fn toml_str_array<S: AsRef<str>>(items: &[S]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let inner = items
        .iter()
        .map(|s| format!("  \"{}\"", s.as_ref()))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("[\n{inner}\n]")
}

/// Environment overrides, applied last so a shell or a systemd unit can steer a
/// run without editing any file. Each entry is `(variable, dotted config key)`.
const ENV_OVERRIDES: &[(&str, &str)] = &[
    ("RAGPILOT_PROJECT_NAME", "project.name"),
    ("RAGPILOT_QDRANT_URL", "qdrant.url"),
    ("RAGPILOT_QDRANT_API_KEY", "qdrant.api_key"),
    ("RAGPILOT_QDRANT_COLLECTION", "qdrant.collection"),
    ("RAGPILOT_EMBEDDING_PROVIDER", "embedding.provider"),
    ("RAGPILOT_LOCAL_MODEL", "embedding.local.model"),
    ("RAGPILOT_API_MODEL", "embedding.api.model"),
];

impl Config {
    /// Load a project config through the full precedence chain:
    /// **env > project config > global config > built-in defaults**.
    ///
    /// The global layer (`<data_root>/config.toml`) is optional and usually
    /// holds only machine-wide settings such as the Qdrant URL or the
    /// embedding model, so every project does not have to repeat them.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read config: {}", path.display()))?;
        let project: toml::Value = toml::from_str(&content)
            .with_context(|| format!("Cannot parse config: {}", path.display()))?;

        let global = Self::load_global_layer()?;
        Self::from_layers(global, Some(project))
            .with_context(|| format!("Cannot parse config: {}", path.display()))
    }

    /// Read `<data_root>/config.toml`, if it exists. A malformed global config
    /// is an error rather than a silent skip — otherwise a typo there would
    /// quietly change every project's behaviour.
    fn load_global_layer() -> Result<Option<toml::Value>> {
        let path = crate::paths::global_config_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Cannot read global config: {}", path.display()))?;
        let value = toml::from_str(&content)
            .with_context(|| format!("Cannot parse global config: {}", path.display()))?;
        Ok(Some(value))
    }

    /// Merge the layers and deserialize. Built-in defaults come from serde, so
    /// anything neither layer sets keeps its `#[serde(default)]` value.
    fn from_layers(global: Option<toml::Value>, project: Option<toml::Value>) -> Result<Self> {
        let mut merged = toml::Value::Table(toml::map::Map::new());
        for layer in [global, project].into_iter().flatten() {
            merge_toml(&mut merged, layer);
        }
        apply_env_overrides(&mut merged, |var| std::env::var(var).ok());
        merged.try_into().map_err(Into::into)
    }

    pub fn default_template(project_name: &str) -> String {
        Self::template_with(project_name, &["rs", "toml", "md"], &["src"])
    }

    /// Build a `config.toml` with caller-chosen `include_extensions` and
    /// `include_dirs` (the rest of the template is fixed). An empty
    /// `include_dirs` slice renders `include_dirs = []`, meaning "scan the
    /// whole project root" (see `indexer::scan_files_with_report`).
    pub fn template_with<S: AsRef<str>>(
        project_name: &str,
        include_extensions: &[S],
        include_dirs: &[S],
    ) -> String {
        let collection = project_name.to_lowercase().replace([' ', '-'], "_");
        let ext_array = toml_str_array(include_extensions);
        let dir_array = toml_str_array(include_dirs);
        format!(
r#"[project]
name = "{project_name}"

[embedding]
provider = "local"

  [embedding.local]
  model = "BAAI/bge-small-en-v1.5"

  [embedding.api]
  provider = "openai"
  model = "text-embedding-3-small"
  api_key_env = "OPENAI_API_KEY"

[qdrant]
url = "http://localhost:6334"
collection = "{collection}"

[indexing]
chunk_size = 700
chunk_overlap = 80
max_file_size_kb = 250
max_parallel_files = 2
embedding_batch_size = 16
max_parallel_embeddings = 1
skip_minified = true
skip_binary = true
include_extensions = {ext_array}
exclude_dirs = [
  ".git",
  ".rag",
  "target",
  "node_modules",
  "__pycache__",
  ".venv",
  "venv",
  "dist",
  "build",
  ".next",
  ".nuxt",
  "vendor",
  "coverage",
  ".cache",
  ".turbo",
  ".idea",
  ".vscode"
]
include_dirs = {dir_array}

[mcp]
context_chunks = 4
max_context_files = 8
max_context_chunks = 20
max_context_tokens = 12000
auto_update_before_search = false
search_tool_description = """
Searches the local project codebase and documentation using semantic similarity.
Call this tool whenever the user asks about: how code works, where something is
implemented, project structure, functions, modules, configuration, or any
project-specific question. Returns relevant code snippets and docs with file paths.
"""

[watcher]
enabled = false
debounce_ms = 2000

[symbol_graph]
enabled = true
max_depth = 2
max_nodes = 200
"#
        )
    }

    pub fn rag_dir(root: &Path) -> PathBuf { root.join(".rag") }
    pub fn state_path(root: &Path) -> PathBuf { Self::rag_dir(root).join("state.json") }
    pub fn config_path(root: &Path) -> PathBuf { Self::rag_dir(root).join("config.toml") }
    pub fn stores_db(root: &Path) -> PathBuf { Self::rag_dir(root).join("stores.db") }
}

/// Deep-merge `overlay` onto `base`: tables merge key by key, everything else
/// (scalars, arrays) is replaced wholesale. Replacing arrays is deliberate —
/// a project that narrows `include_extensions` must not inherit the global list.
fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(slot) => merge_toml(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, overlay) => *slot = overlay,
    }
}

/// Apply [`ENV_OVERRIDES`] onto a merged config table. `read` is injected so
/// the mapping can be tested without touching process-wide environment state.
fn apply_env_overrides(root: &mut toml::Value, read: impl Fn(&str) -> Option<String>) {
    for (var, dotted) in ENV_OVERRIDES {
        let Some(raw) = read(var).filter(|v| !v.is_empty()) else { continue };
        set_dotted(root, dotted, toml::Value::String(raw));
    }
}

/// Set `a.b.c` in a TOML table, creating intermediate tables as needed.
fn set_dotted(root: &mut toml::Value, dotted: &str, value: toml::Value) {
    let mut cursor = root;
    let mut parts = dotted.split('.').peekable();

    while let Some(part) = parts.next() {
        if !cursor.is_table() {
            *cursor = toml::Value::Table(toml::map::Map::new());
        }
        let table = cursor.as_table_mut().expect("just ensured a table");

        if parts.peek().is_none() {
            table.insert(part.to_string(), value);
            return;
        }
        cursor = table
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml_value(src: &str) -> toml::Value {
        toml::from_str(src).unwrap()
    }

    const PROJECT: &str = r#"
[project]
name = "demo"

[embedding]
provider = "local"

[qdrant]
url = "http://localhost:6334"

[indexing]
chunk_size = 400
chunk_overlap = 50
include_extensions = ["rs"]

[mcp]
context_chunks = 4
"#;

    #[test]
    fn project_layer_wins_over_global() {
        let global = toml_value(
            r#"
[qdrant]
url = "http://qdrant.internal:6334"
api_key = "from-global"

[indexing]
chunk_size = 999
include_extensions = ["rs", "py", "md"]
"#,
        );

        let cfg = Config::from_layers(Some(global), Some(toml_value(PROJECT))).unwrap();

        // Project overrides the global value…
        assert_eq!(cfg.qdrant.url, "http://localhost:6334");
        assert_eq!(cfg.indexing.chunk_size, 400);
        // …arrays are replaced, not appended.
        assert_eq!(cfg.indexing.include_extensions, vec!["rs".to_string()]);
        // …and keys the project never mentions still come from the global layer.
        assert_eq!(cfg.qdrant.api_key.as_deref(), Some("from-global"));
    }

    #[test]
    fn global_layer_wins_over_builtin_defaults() {
        let global = toml_value(
            r#"
[embedding.local]
model = "BAAI/bge-base-en-v1.5"
"#,
        );
        let cfg = Config::from_layers(Some(global), Some(toml_value(PROJECT))).unwrap();
        assert_eq!(cfg.embedding.local.model, "BAAI/bge-base-en-v1.5");

        // With no layer setting it, the serde default stands.
        let bare = Config::from_layers(None, Some(toml_value(PROJECT))).unwrap();
        assert_eq!(bare.embedding.local.model, "BAAI/bge-small-en-v1.5");
    }

    #[test]
    fn env_wins_over_every_file_layer() {
        let mut merged = toml_value(PROJECT);
        merge_toml(&mut merged, toml_value("[qdrant]\nurl = \"http://global:6334\""));
        apply_env_overrides(&mut merged, |var| match var {
            "RAGPILOT_QDRANT_URL" => Some("http://env:6334".to_string()),
            "RAGPILOT_LOCAL_MODEL" => Some("BAAI/bge-large-en-v1.5".to_string()),
            // An empty value counts as unset.
            "RAGPILOT_PROJECT_NAME" => Some(String::new()),
            _ => None,
        });

        let cfg: Config = merged.try_into().unwrap();
        assert_eq!(cfg.qdrant.url, "http://env:6334");
        assert_eq!(cfg.embedding.local.model, "BAAI/bge-large-en-v1.5");
        assert_eq!(cfg.project.name, "demo");
    }

    #[test]
    fn set_dotted_creates_missing_tables() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_dotted(&mut root, "a.b.c", toml::Value::String("v".into()));
        assert_eq!(root["a"]["b"]["c"].as_str(), Some("v"));

        // A scalar standing where a table is needed is replaced, not panicked on.
        set_dotted(&mut root, "a.b", toml::Value::String("scalar".into()));
        set_dotted(&mut root, "a.b.d", toml::Value::String("w".into()));
        assert_eq!(root["a"]["b"]["d"].as_str(), Some("w"));
    }
}

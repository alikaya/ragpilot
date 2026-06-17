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

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read config: {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("Cannot parse config: {}", path.display()))
    }

    pub fn default_template(project_name: &str) -> String {
        let collection = project_name.to_lowercase().replace([' ', '-'], "_");
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
include_extensions = [
  "rs",
  "toml",
  "md"
]
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
include_dirs = [
  "src"
]

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

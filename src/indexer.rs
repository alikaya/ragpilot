use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::{Config, IndexingConfig};
use crate::embedder::Embedder;
use crate::store::{Chunk, VectorStore};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkipStats {
    #[serde(default)]
    pub large_file: usize,
    #[serde(default)]
    pub binary: usize,
    #[serde(default)]
    pub minified: usize,
    #[serde(default)]
    pub excluded_dir: usize,
    #[serde(default)]
    pub unsupported_extension: usize,
}

impl SkipStats {
    pub fn total(&self) -> usize {
        self.large_file + self.binary + self.minified + self.excluded_dir + self.unsupported_extension
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleTokenStats {
    #[serde(default)]
    pub task: String,
    pub generated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms: u128,
    #[serde(default)]
    pub estimated_tokens: usize,
    /// Honest baseline: cost of reading the matched files whole (no-RAG case).
    #[serde(default)]
    pub full_file_baseline_tokens: usize,
    #[serde(default)]
    pub saving_vs_full_file_tokens: usize,
    #[serde(default)]
    pub saving_vs_full_file_percent: f64,
    #[serde(default)]
    pub saving_ratio: f64,
    #[serde(default)]
    pub candidate_chunks_estimated_tokens: usize,
    #[serde(default)]
    pub selected_chunks_estimated_tokens: usize,
    /// Tuning-only: fraction of retrieved chunks dropped by the budget cap.
    #[serde(default)]
    pub budget_trim_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexState {
    pub indexed_at: Option<DateTime<Utc>>,
    pub file_hashes: HashMap<String, String>,
    pub total_chunks: usize,
    pub total_files: usize,
    pub embedding_model: String,
    pub embedding_provider: String,
    #[serde(default)]
    pub skipped: SkipStats,
    #[serde(default)]
    pub last_bundle_token_stats: Option<BundleTokenStats>,
}

pub struct IndexRunLock {
    path: PathBuf,
}

impl Drop for IndexRunLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn try_acquire_index_lock(root: &Path) -> Result<IndexRunLock> {
    let lock_path = Config::rag_dir(root).join("index.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match std::fs::OpenOptions::new().create_new(true).write(true).open(&lock_path) {
        Ok(mut f) => {
            let _ = writeln!(f, "pid={} started_at={}", std::process::id(), Utc::now().to_rfc3339());
            Ok(IndexRunLock { path: lock_path })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!("indexing already running")
        }
        Err(e) => Err(e.into()),
    }
}

impl IndexState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read state: {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| "Failed to parse state.json")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub files: Vec<PathBuf>,
    pub skipped: SkipStats,
}

pub fn scan_files_with_report(root: &Path, config: &IndexingConfig) -> Result<ScanReport> {
    let mut skipped = SkipStats::default();
    let mut files = Vec::new();

    if config.include_dirs.is_empty() {
        scan_files_from(root, root, config, &mut files, &mut skipped)?;
    } else {
        for dir in &config.include_dirs {
            let abs_dir = root.join(dir);
            if abs_dir.is_dir() {
                scan_files_from(&abs_dir, root, config, &mut files, &mut skipped)?;
            } else {
                tracing::debug!("include_dirs: '{}' not found, skipping", dir);
            }
        }
    }

    Ok(ScanReport { files, skipped })
}

pub fn scan_files(root: &Path, config: &IndexingConfig) -> Result<Vec<PathBuf>> {
    Ok(scan_files_with_report(root, config)?.files)
}

fn scan_files_from(
    start: &Path,
    root: &Path,
    config: &IndexingConfig,
    files: &mut Vec<PathBuf>,
    skipped: &mut SkipStats,
) -> Result<()> {
    let _ = root;

    let ext_set: HashSet<String> = config.include_extensions.iter().map(|e| e.to_lowercase()).collect();

    for entry in WalkDir::new(start)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                let excluded = config.exclude_dirs.iter().any(|d| name == d.as_str());
                return !excluded;
            }
            true
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Walkdir error: {e}");
                continue;
            }
        };

        if entry.file_type().is_dir() {
            let name = entry.file_name().to_string_lossy();
            if config.exclude_dirs.iter().any(|d| name == d.as_str()) {
                skipped.excluded_dir += 1;
            }
            continue;
        }

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path().to_path_buf();

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !ext_set.contains(ext.as_str()) {
            skipped.unsupported_extension += 1;
            continue;
        }

        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size_kb = meta.len() / 1024;
        if size_kb > config.max_file_size_kb {
            skipped.large_file += 1;
            continue;
        }

        let sample = std::fs::read(&path).unwrap_or_default();
        if config.skip_binary && looks_binary(&sample) {
            skipped.binary += 1;
            continue;
        }

        if config.skip_minified && looks_minified(&path, &sample) {
            skipped.minified += 1;
            continue;
        }

        files.push(path);
    }

    Ok(())
}

fn looks_binary(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    if data.contains(&0) {
        return true;
    }
    std::str::from_utf8(data).is_err()
}

fn looks_minified(path: &Path, data: &[u8]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.ends_with(".min.js") || name.ends_with(".min.css") {
        return true;
    }
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let chars = text.chars().count();
    let lines = text.lines().count().max(1);
    let max_line = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    max_line >= 20_000 || (lines <= 5 && chars >= 20_000)
}

pub fn chunk_text(content: &str, chunk_size: usize, overlap: usize) -> Vec<(String, usize, usize)> {
    let chars: Vec<char> = content.chars().collect();
    let total = chars.len();

    if total == 0 {
        return vec![];
    }
    if total <= chunk_size {
        let end_line = chars.iter().filter(|&&c| c == '\n').count() + 1;
        return vec![(content.to_string(), 1, end_line)];
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < total {
        let end = (start + chunk_size).min(total);
        let chunk: String = chars[start..end].iter().collect();

        let start_line = chars[..start].iter().filter(|&&c| c == '\n').count() + 1;
        let end_line = start_line + chunk.chars().filter(|&c| c == '\n').count();

        chunks.push((chunk, start_line, end_line));

        if end == total {
            break;
        }
        start = if end > overlap { end - overlap } else { end };
    }

    chunks
}

pub fn extract_symbol(content: &str, language: &str) -> Option<String> {
    let prefixes: &[&str] = match language {
        "rust" => &[
            "pub async fn ", "pub fn ", "async fn ", "fn ", "pub struct ", "struct ", "pub enum ", "enum ",
            "pub trait ", "trait ", "impl ",
        ],
        "python" => &["async def ", "def ", "class "],
        "javascript" | "typescript" => &[
            "export async function ", "export function ", "async function ", "function ", "export class ", "class ",
            "export const ", "const ",
        ],
        "go" => &["func "],
        "java" | "kotlin" | "scala" | "csharp" => {
            &["public class ", "private class ", "protected class ", "class ", "public static ", "public "]
        }
        "ruby" => &["def ", "class "],
        "php" => &["function ", "class "],
        "swift" => &["func ", "class ", "struct "],
        _ => return None,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        for prefix in prefixes {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    return Some(format!("{}{}", prefix.trim_start(), name));
                }
            }
        }
    }
    None
}

pub fn file_language(extension: &str) -> &'static str {
    match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cxx" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "shell",
        "md" | "mdx" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "xml" => "xml",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        "sql" => "sql",
        "dockerfile" => "dockerfile",
        _ => "text",
    }
}

pub fn content_type_for(extension: &str) -> &'static str {
    match extension {
        "md" | "mdx" | "txt" | "rst" => "documentation",
        "toml" | "yaml" | "yml" | "json" | "xml" | "ini" | "cfg" | "conf" => "config",
        _ => "code",
    }
}

pub fn compute_hash(content: &str) -> String {
    format!("{:x}", md5::compute(content.as_bytes()))
}

pub async fn index_project(
    root: &Path,
    config: &Config,
    embedder: &dyn Embedder,
    store: &dyn VectorStore,
    force: bool,
) -> Result<IndexState> {
    let _guard = try_acquire_index_lock(root)?;
    let state_path = Config::state_path(root);
    let mut state = if force { IndexState::default() } else { IndexState::load(&state_path)? };

    let scan = scan_files_with_report(root, &config.indexing)?;
    let files = scan.files;
    state.skipped = scan.skipped;
    tracing::info!("Found {} files to consider", files.len());

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap(),
    );

    let dim = embedder.dimension() as u64;
    store.ensure_collection(dim).await?;

    let mut total_new_chunks = 0usize;
    let mut files_indexed = 0usize;

    for file_path in &files {
        let rel = file_path.strip_prefix(root).unwrap_or(file_path);
        let rel_str = rel.to_string_lossy().to_string();

        pb.set_message(format!("Indexing {}", rel_str));

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => {
                pb.inc(1);
                continue;
            }
        };

        let hash = compute_hash(&content);
        if !force {
            if let Some(existing_hash) = state.file_hashes.get(&rel_str) {
                if *existing_hash == hash {
                    pb.inc(1);
                    continue;
                }
            }
        }

        store.delete_by_source(&rel_str).await.unwrap_or_else(|e| tracing::warn!("delete_by_source error: {e}"));

        let ext = file_path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let language = file_language(&ext).to_string();
        let ct = content_type_for(&ext).to_string();
        let raw_chunks = chunk_text(&content, config.indexing.chunk_size, config.indexing.chunk_overlap);
        let chunk_total = raw_chunks.len();

        if raw_chunks.is_empty() {
            state.file_hashes.insert(rel_str, hash);
            pb.inc(1);
            continue;
        }

        let texts: Vec<String> = raw_chunks.iter().map(|(t, _, _)| t.clone()).collect();
        let vectors = embedder.embed(&texts).await.with_context(|| format!("Embedding failed for {}", rel_str))?;

        let chunks: Vec<Chunk> = raw_chunks
            .into_iter()
            .enumerate()
            .map(|(i, (text, start_line, end_line))| {
                let symbol = extract_symbol(&text, &language);
                Chunk {
                    id: format!("{}:{}", rel_str, i),
                    content: text,
                    source: rel_str.clone(),
                    chunk_index: i,
                    chunk_total,
                    start_line,
                    end_line,
                    file_hash: hash.clone(),
                    content_type: ct.clone(),
                    language: language.clone(),
                    symbol,
                }
            })
            .collect();

        store.upsert_chunks(&chunks, &vectors).await.with_context(|| format!("Upsert failed for {}", rel_str))?;

        state.file_hashes.insert(rel_str, hash);
        total_new_chunks += chunk_total;
        files_indexed += 1;
        pb.inc(1);
    }

    pb.finish_with_message(format!("Indexed {} files, {} new chunks", files_indexed, total_new_chunks));

    state.indexed_at = Some(Utc::now());
    state.total_files = state.file_hashes.len();
    state.total_chunks += total_new_chunks;
    state.save(&state_path)?;

    Ok(state)
}

pub async fn index_file(
    root: &Path,
    file_path: &Path,
    config: &Config,
    embedder: &dyn Embedder,
    store: &dyn VectorStore,
    state: &mut IndexState,
) -> Result<()> {
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    let rel_str = rel.to_string_lossy().to_string();

    let content = std::fs::read_to_string(file_path).with_context(|| format!("Cannot read {}", file_path.display()))?;
    let hash = compute_hash(&content);

    if let Some(existing) = state.file_hashes.get(&rel_str) {
        if *existing == hash {
            return Ok(());
        }
    }

    store.delete_by_source(&rel_str).await?;

    let ext = file_path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let language = file_language(&ext).to_string();
    let ct = content_type_for(&ext).to_string();

    let raw_chunks = chunk_text(&content, config.indexing.chunk_size, config.indexing.chunk_overlap);
    let chunk_total = raw_chunks.len();

    if raw_chunks.is_empty() {
        state.file_hashes.insert(rel_str, hash);
        return Ok(());
    }

    let texts: Vec<String> = raw_chunks.iter().map(|(t, _, _)| t.clone()).collect();
    let vectors = embedder.embed(&texts).await?;

    let chunks: Vec<Chunk> = raw_chunks
        .into_iter()
        .enumerate()
        .map(|(i, (text, start_line, end_line))| {
            let symbol = extract_symbol(&text, &language);
            Chunk {
                id: format!("{}:{}", rel_str, i),
                content: text,
                source: rel_str.clone(),
                chunk_index: i,
                chunk_total,
                start_line,
                end_line,
                file_hash: hash.clone(),
                content_type: ct.clone(),
                language: language.clone(),
                symbol,
            }
        })
        .collect();

    store.upsert_chunks(&chunks, &vectors).await?;
    state.file_hashes.insert(rel_str, hash);
    state.total_chunks += chunk_total;

    Ok(())
}

fn current_root() -> Result<PathBuf> {
    std::env::current_dir().map_err(|e| anyhow::anyhow!("Cannot determine working directory: {e}"))
}

fn resolve_store(config: &Config) -> Result<crate::store::qdrant::QdrantStore> {
    let mut cfg = config.qdrant.clone();
    cfg.collection = Some(cfg.collection_name(&config.project.name));
    crate::store::qdrant::QdrantStore::new(&cfg)
}

fn build_orchestrator(root: &PathBuf, config: &Config) -> Result<crate::orchestrator::IndexOrchestrator> {
    use std::sync::Arc;

    let db_path = Config::stores_db(root);
    crate::store::sqlite::SqliteStore::new(db_path.clone())?;

    let embedder: Arc<dyn crate::embedder::Embedder> = Arc::from(crate::embedder::create(&config.embedding)?);
    let vector_store: Arc<dyn crate::store::VectorStore> = Arc::new(resolve_store(config)?);
    let symbol_graph = Arc::new(crate::store::symbol_graph::SymbolGraphStore::new(db_path.clone()));
    let project_tree = Arc::new(crate::store::project_tree::ProjectTreeStore::new(db_path.clone()));
    let impact_index = Arc::new(crate::store::impact_index::ImpactIndexStore::new(db_path));

    Ok(crate::orchestrator::IndexOrchestrator::new(
        Arc::new(config.clone()),
        root.clone(),
        embedder,
        vector_store,
        symbol_graph,
        project_tree,
        impact_index,
    ))
}

pub async fn cmd_init(force: bool) -> Result<()> {
    use colored::Colorize;

    let root = current_root()?;
    let rag_dir = Config::rag_dir(&root);
    let config_path = Config::config_path(&root);

    if !config_path.exists() || force {
        std::fs::create_dir_all(&rag_dir)?;
        let project_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        std::fs::write(&config_path, Config::default_template(&project_name))?;
        println!("{} Created {}", "✓".green(), config_path.display());
    } else {
        println!("{} Config already exists: {}", "i".blue(), config_path.display());
    }

    let config = Config::load(&config_path)?;
    println!("{} Indexing project '{}'…", "→".cyan(), config.project.name);

    let orch = build_orchestrator(&root, &config)?;
    let result = orch.ensure_index_with_progress(force).await?;

    println!(
        "{} Done — {} files indexed ({} dirty), {}ms.",
        "✓".green(), result.indexed, result.dirty_count, result.duration_ms
    );
    Ok(())
}

pub async fn cmd_update() -> Result<()> {
    use colored::Colorize;

    let root = current_root()?;
    let config =
        Config::load(&Config::config_path(&root)).map_err(|_| anyhow::anyhow!("No .rag/config.toml found. Run 'rag init' first."))?;

    let state = IndexState::load(&Config::state_path(&root))?;
    println!("{} Checking for changes ({} files in index)…", "→".cyan(), state.file_hashes.len());

    let orch = build_orchestrator(&root, &config)?;
    let result = orch.ensure_index_with_progress(false).await?;

    if result.dirty_count == 0 {
        println!("{} Already up to date — no changes detected.", "✓".green());
    } else {
        println!(
            "{} Updated — {}/{} changed files re-indexed, {}ms.",
            "✓".green(), result.indexed, result.dirty_count, result.duration_ms
        );
    }
    Ok(())
}

pub async fn cmd_status() -> Result<()> {
    use colored::Colorize;

    let root = current_root()?;
    let config =
        Config::load(&Config::config_path(&root)).map_err(|_| anyhow::anyhow!("No .rag/config.toml found. Run 'rag init' first."))?;
    let state = IndexState::load(&Config::state_path(&root))?;

    println!("{}", "─── Project ─────────────────────────────".bold());
    println!("  Name:     {}", config.project.name);
    println!("  Root:     {}", root.display());

    println!("\n{}", "─── Index ───────────────────────────────".bold());
    if let Some(t) = &state.indexed_at {
        println!("  Last indexed: {}", t.format("%Y-%m-%d %H:%M:%S UTC"));
    } else {
        println!("  Last indexed: {}", "never".yellow());
    }
    println!("  Files indexed: {}", state.total_files);
    println!("  Chunks:        ~{}", state.total_chunks);
    println!("  Skipped files: {}", state.skipped.total());
    println!("    large file:            {}", state.skipped.large_file);
    println!("    binary:                {}", state.skipped.binary);
    println!("    minified:              {}", state.skipped.minified);
    println!("    excluded dir:          {}", state.skipped.excluded_dir);
    println!("    unsupported extension: {}", state.skipped.unsupported_extension);
    if !state.embedding_model.is_empty() {
        println!("  Embedding model: {} ({})", state.embedding_model, state.embedding_provider);
    }

    let dirty = state
        .file_hashes
        .iter()
        .filter(|(rel, stored_hash)| match std::fs::read_to_string(root.join(rel.as_str())) {
            Ok(c) => &compute_hash(&c) != *stored_hash,
            Err(_) => true,
        })
        .count();
    println!("  Dirty files:   {}", dirty);

    println!("\n{}", "─── Qdrant ──────────────────────────────".bold());
    let store = resolve_store(&config)?;
    match store.collection_info().await {
        Ok(info) => {
            println!("  URL:        {}", config.qdrant.url);
            println!("  Collection: {}", info.name);
            println!("  Points:     {}", info.points_count);
            println!("  Dim:        {}", info.dimension);
        }
        Err(e) => {
            println!("  {} Qdrant unreachable: {}", "✗".red(), e);
        }
    }

    println!("\n{}", "─── MCP ─────────────────────────────────".bold());
    println!("  context_chunks:     {}", config.mcp.context_chunks);
    println!("  max_context_files:  {}", config.mcp.max_context_files);
    println!("  max_context_chunks: {}", config.mcp.max_context_chunks);
    println!("  max_context_tokens: {}", config.mcp.max_context_tokens);
    println!("  Run:                rag --mcp-server");

    Ok(())
}

pub async fn cmd_stats() -> Result<()> {
    use colored::Colorize;

    let root = current_root()?;
    let state = IndexState::load(&Config::state_path(&root))?;

    const LW: usize = 28;
    const VW: usize = 42;
    let lh = "─".repeat(LW + 2);
    let vh = "─".repeat(VW + 2);
    let row = |label: &str, value: String| {
        let v: String = value.chars().take(VW).collect();
        println!("│ {:<LW$} │ {:<VW$} │", label, v);
    };

    println!("{}", "RAG Stats — last context.bundle".bold());
    match &state.last_bundle_token_stats {
        Some(s) => {
            let at = s
                .generated_at
                .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let secs = s.duration_ms / 1000;

            println!("┌{}┬{}┐", lh, vh);
            row("Metric", "Value".to_string());
            println!("├{}┼{}┤", lh, vh);
            row("Generated at", at);
            row("Task", s.task.clone());
            row("Duration", format!("{}s", secs));
            row("Bundle delivered", format!("{} tokens", s.estimated_tokens));
            row(
                "Full-file baseline (no-RAG)",
                format!("{} tokens", s.full_file_baseline_tokens),
            );
            row(
                "Saving vs full-file read",
                format!("{} tokens", s.saving_vs_full_file_tokens),
            );
            row("Saving percent", format!("{:.2}%", s.saving_vs_full_file_percent));
            row("Saving ratio", format!("{:.2}x", s.saving_ratio));
            row("Budget trim (tuning only)", format!("{:.2}%", s.budget_trim_percent));
            println!("└{}┴{}┘", lh, vh);
            println!(
                "{}",
                "Note: baseline is an upper bound vs naive full-file reads; excludes follow-up calls.".dimmed()
            );
        }
        None => {
            println!("No context.bundle stats recorded yet.");
        }
    }
    Ok(())
}

pub async fn cmd_clean(yes: bool) -> Result<()> {
    use colored::Colorize;

    let root = current_root()?;
    let config = Config::load(&Config::config_path(&root)).map_err(|_| anyhow::anyhow!("No .rag/config.toml found."))?;
    let coll = config.qdrant.collection_name(&config.project.name);

    if !yes {
        print!("{} Delete collection '{}'? [y/N] ", "!".yellow(), coll);
        use std::io::{BufRead, Write};
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let store = resolve_store(&config)?;
    store.delete_collection().await?;
    IndexState::default().save(&Config::state_path(&root))?;
    println!("{} Collection '{}' deleted.", "✓".green(), coll);
    Ok(())
}

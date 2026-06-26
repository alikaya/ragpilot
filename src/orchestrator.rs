use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::embedder::Embedder;
use crate::indexer::{chunk_text, compute_hash, content_type_for, extract_symbol, file_language};
use crate::parser::{Parser, TreeSitterParser};
use crate::store::impact_index::ImpactIndexStore;
use crate::store::project_tree::{node_from_path, ProjectTreeStore};
use crate::store::symbol_graph::SymbolGraphStore;
use crate::store::{Chunk, VectorStore};

pub struct EnsureIndexResult {
    pub dirty_count: usize,
    pub indexed:     usize,
    pub duration_ms: u128,
}

pub struct IndexOrchestrator {
    pub config:       Arc<Config>,
    pub root:         PathBuf,
    pub embedder:     Arc<dyn Embedder>,
    pub vector_store: Arc<dyn VectorStore>,
    pub symbol_graph: Arc<SymbolGraphStore>,
    pub project_tree: Arc<ProjectTreeStore>,
    pub impact_index: Arc<ImpactIndexStore>,
    parser:           Arc<TreeSitterParser>,
}

impl IndexOrchestrator {
    pub fn new(
        config:       Arc<Config>,
        root:         PathBuf,
        embedder:     Arc<dyn Embedder>,
        vector_store: Arc<dyn VectorStore>,
        symbol_graph: Arc<SymbolGraphStore>,
        project_tree: Arc<ProjectTreeStore>,
        impact_index: Arc<ImpactIndexStore>,
    ) -> Self {
        // Per-project query overrides live under `.rag/queries/<lang>/`.
        let parser = Arc::new(TreeSitterParser::with_query_overrides(&root.join(".rag/queries")));
        Self {
            config, root, embedder, vector_store,
            symbol_graph, project_tree, impact_index,
            parser,
        }
    }

    /// Whether a given path should be indexed (include_dirs + exclude_dirs + extension check).
    pub fn should_index(&self, path: &Path) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);

        // If include_dirs is set, the file must live inside one of them
        if !self.config.indexing.include_dirs.is_empty() {
            if !self.config.indexing.include_dirs.iter().any(|d| rel.starts_with(d)) {
                return false;
            }
        }

        // Check excluded dirs
        for component in rel.components() {
            let name = component.as_os_str().to_string_lossy();
            if self.config.indexing.exclude_dirs.iter().any(|d| d == name.as_ref()) {
                return false;
            }
        }

        // Check extension
        let ext = path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        self.config.indexing.include_extensions.iter().any(|e| e == ext.as_str())
    }

    /// Process a single file: re-index iff content changed.
    pub async fn process_file(&self, path: &Path) -> Result<bool> {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().to_string();

        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => {
                // File deleted — clean up stores
                self.remove_file(&rel_str).await;
                return Ok(true);
            }
        };

        let hash = compute_hash(&content);

        // Load state to check hash
        let state_path = Config::state_path(&self.root);
        let mut state  = crate::indexer::IndexState::load(&state_path)?;

        if !matches!(state.file_hashes.get(&rel_str), Some(h) if h == &hash) {
            self.reindex_file(&rel_str, path, &content, &hash).await?;
            state.file_hashes.insert(rel_str.clone(), hash);
            state.save(&state_path)?;
        }

        Ok(true)
    }

    async fn reindex_file(&self, rel_str: &str, abs_path: &Path, content: &str, hash: &str) -> Result<()> {
        let ext      = abs_path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let language = file_language(&ext).to_string();
        let ct       = content_type_for(&ext).to_string();

        // --- Vector index ---
        let raw_chunks = chunk_text(content, self.config.indexing.chunk_size, self.config.indexing.chunk_overlap);
        let chunk_total = raw_chunks.len();

        if !raw_chunks.is_empty() {
            let texts: Vec<String> = raw_chunks.iter().map(|(t, _, _)| t.clone()).collect();
            let mut vectors = Vec::with_capacity(texts.len());
            let batch_size = self.config.indexing.embedding_batch_size.max(1);
            let max_parallel = self.config.indexing.max_parallel_embeddings.max(1);
            let mut batches: VecDeque<(usize, Vec<String>)> = texts
                .chunks(batch_size)
                .enumerate()
                .map(|(i, c)| (i, c.to_vec()))
                .collect();

            while !batches.is_empty() {
                let mut set = JoinSet::new();
                for _ in 0..max_parallel {
                    if let Some((idx, batch)) = batches.pop_front() {
                        let embedder = Arc::clone(&self.embedder);
                        set.spawn(async move { (idx, embedder.embed(&batch).await) });
                    }
                }
                let mut ordered = Vec::new();
                while let Some(done) = set.join_next().await {
                    let (idx, out) = done?;
                    let out = out?;
                    ordered.push((idx, out));
                }
                ordered.sort_by_key(|(idx, _)| *idx);
                for (_, mut out) in ordered {
                    vectors.append(&mut out);
                }
            }

            let chunks: Vec<Chunk> = raw_chunks.into_iter().enumerate().map(|(i, (text, start_line, end_line))| {
                let symbol = extract_symbol(&text, &language);
                Chunk {
                    id:           format!("{}:{}", rel_str, i),
                    content:      text,
                    source:       rel_str.to_string(),
                    chunk_index:  i,
                    chunk_total,
                    start_line,
                    end_line,
                    file_hash:    hash.to_string(),
                    content_type: ct.clone(),
                    language:     language.clone(),
                    symbol,
                }
            }).collect();

            self.vector_store.delete_by_source(rel_str).await
                .unwrap_or_else(|e| tracing::warn!("delete_by_source: {e}"));
            self.vector_store.upsert_chunks(&chunks, &vectors).await?;
        }

        // --- Symbol graph ---
        if self.config.symbol_graph.enabled {
            let parsed = self.parser.parse(rel_str, content, &language);
            self.symbol_graph.upsert(rel_str, &parsed.symbols, &parsed.imports, &parsed.calls).await
                .unwrap_or_else(|e| tracing::warn!("symbol_graph upsert: {e}"));

            // Impact index: record what this file imports
            let imported: Vec<String> = parsed.imports.iter().map(|i| i.from_module.clone()).collect();
            self.impact_index.update_imports(rel_str, &imported).await
                .unwrap_or_else(|e| tracing::warn!("impact_index update: {e}"));
        }

        // --- Project tree ---
        let node = node_from_path(abs_path, &self.root, hash);
        self.project_tree.upsert(node).await
            .unwrap_or_else(|e| tracing::warn!("project_tree upsert: {e}"));

        tracing::debug!("Reindexed: {}", rel_str);
        Ok(())
    }

    async fn remove_file(&self, rel_str: &str) {
        let _ = self.vector_store.delete_by_source(rel_str).await;
        let _ = self.project_tree.remove(rel_str).await;
        let _ = self.symbol_graph.remove(rel_str).await;
    }

    /// Scan all files, find dirty ones, reindex them (vector + symbol graph + tree).
    /// `show_progress`: display a terminal progress bar (false in MCP server mode).
    pub async fn ensure_index(&self, force: bool) -> Result<EnsureIndexResult> {
        self.ensure_index_inner(force, false).await
    }

    pub async fn ensure_index_with_progress(&self, force: bool) -> Result<EnsureIndexResult> {
        self.ensure_index_inner(force, true).await
    }

    async fn ensure_index_inner(&self, force: bool, show_progress: bool) -> Result<EnsureIndexResult> {
        let start = Instant::now();
        let _index_lock = crate::indexer::try_acquire_index_lock(&self.root)?;

        let state_path = Config::state_path(&self.root);
        let state      = crate::indexer::IndexState::load(&state_path)?;

        let scan = crate::indexer::scan_files_with_report(&self.root, &self.config.indexing)?;
        let files = scan.files;
        let total = files.len();

        let pb = if show_progress {
            let bar = ProgressBar::new(total as u64);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
                    .unwrap()
            );
            Some(bar)
        } else {
            None
        };

        // Ensure Qdrant collection exists
        let dim = self.embedder.dimension() as u64;
        self.vector_store.ensure_collection(dim).await?;

        let mut dirty_count = 0usize;
        let mut indexed     = 0usize;
        let mut new_hashes  = state.file_hashes.clone();

        // Remove stale entries: files that were deleted or moved outside include_dirs
        let scanned: std::collections::HashSet<String> = files.iter()
            .map(|p| p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().to_string())
            .collect();
        let stale: Vec<String> = new_hashes.keys()
            .filter(|k| !scanned.contains(*k))
            .cloned()
            .collect();
        for s in stale {
            tracing::debug!("Removing stale entry: {}", s);
            self.remove_file(&s).await;
            new_hashes.remove(&s);
        }

        // Self-heal the symbol graph: drop any orphaned file whose path is no
        // longer scanned (e.g. left behind after a delete diverged from state).
        if self.config.symbol_graph.enabled {
            let keep: Vec<String> = scanned.iter().cloned().collect();
            if let Ok(n) = self.symbol_graph.prune_except(keep).await {
                if n > 0 {
                    tracing::debug!("Pruned {n} orphaned file(s) from symbol graph");
                }
            }
        }

        let max_parallel_files = self.config.indexing.max_parallel_files.max(1);
        for group in files.chunks(max_parallel_files) {
            for abs_path in group {
                let rel = abs_path.strip_prefix(&self.root).unwrap_or(abs_path);
                let rel_str = rel.to_string_lossy().to_string();

                if let Some(ref bar) = pb {
                    bar.set_message(rel_str.clone());
                }

                let content = match std::fs::read_to_string(abs_path) {
                    Ok(c) => c,
                    Err(_) => {
                        if let Some(ref bar) = pb {
                            bar.inc(1);
                        }
                        continue;
                    }
                };
                let hash = compute_hash(&content);

                let is_dirty = force || !matches!(state.file_hashes.get(&rel_str), Some(h) if h == &hash);

                if is_dirty {
                    dirty_count += 1;
                    match self.reindex_file(&rel_str, abs_path, &content, &hash).await {
                        Ok(_) => {
                            indexed += 1;
                            new_hashes.insert(rel_str, hash); // only update hash on success
                        }
                        Err(e) => {
                            // keep old hash so next `rag update` retries this file
                            tracing::warn!("reindex failed for '{}': {e}", rel_str);
                        }
                    }
                }

                if let Some(ref bar) = pb {
                    bar.inc(1);
                }
            }
        }

        if let Some(bar) = pb {
            bar.finish_with_message(format!("{} files indexed", indexed));
        }

        // Save state
        let mut state          = state;
        state.file_hashes      = new_hashes;
        state.total_files      = state.file_hashes.len();
        state.total_chunks     += indexed * 5; // approximate; refreshed on full reindex
        state.skipped          = scan.skipped;
        state.indexed_at       = Some(chrono::Utc::now());
        state.embedding_model  = match self.config.embedding.provider.as_str() {
            "api" => self.config.embedding.api.model.clone(),
            _     => self.config.embedding.local.model.clone(),
        };
        state.embedding_provider = self.config.embedding.provider.clone();
        state.save(&state_path)?;

        Ok(EnsureIndexResult { dirty_count, indexed, duration_ms: start.elapsed().as_millis() })
    }
}

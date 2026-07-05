use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::sync::Arc;

use crate::config::LocalEmbeddingConfig;
use super::Embedder;

struct ModelInfo {
    model: EmbeddingModel,
    dimension: usize,
}

fn resolve_model(name: &str) -> Result<ModelInfo> {
    match name {
        "BAAI/bge-small-en-v1.5" => Ok(ModelInfo { model: EmbeddingModel::BGESmallENV15, dimension: 384 }),
        "BAAI/bge-base-en-v1.5" => Ok(ModelInfo { model: EmbeddingModel::BGEBaseENV15, dimension: 768 }),
        "BAAI/bge-large-en-v1.5" => Ok(ModelInfo { model: EmbeddingModel::BGELargeENV15, dimension: 1024 }),
        "nomic-ai/nomic-embed-text-v1.5" => Ok(ModelInfo { model: EmbeddingModel::NomicEmbedTextV15, dimension: 768 }),
        "nomic-ai/nomic-embed-text-v1" => Ok(ModelInfo { model: EmbeddingModel::NomicEmbedTextV1, dimension: 768 }),
        "sentence-transformers/all-MiniLM-L6-v2" => Ok(ModelInfo { model: EmbeddingModel::AllMiniLML6V2, dimension: 384 }),
        _ => Err(anyhow!(
            "Unsupported local model: '{}'. Supported models:\n  \
             BAAI/bge-small-en-v1.5 (dim=384, default)\n  \
             BAAI/bge-base-en-v1.5 (dim=768)\n  \
             BAAI/bge-large-en-v1.5 (dim=1024)\n  \
             nomic-ai/nomic-embed-text-v1.5 (dim=768)\n  \
             sentence-transformers/all-MiniLM-L6-v2 (dim=384)",
            name
        )),
    }
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]);
        }
    }
    std::path::PathBuf::from(path)
}

/// Resolve the model cache directory deterministically — never relative to the
/// process working directory (a server launched with `--root` from $HOME must
/// find the same cache as one launched inside the project):
/// 1. `embedding.local.cache_dir` from config (tilde-expanded),
/// 2. `<project root>/.fastembed_cache` when it already exists (projects that
///    downloaded there before keep working offline),
/// 3. a user-level shared cache (`~/.cache/ragpilot/models`), so the ~130MB
///    model is downloaded once per machine instead of once per directory.
pub fn resolve_cache_dir(config: &LocalEmbeddingConfig, root: &std::path::Path) -> std::path::PathBuf {
    if let Some(ref cache_dir) = config.cache_dir {
        return expand_tilde(cache_dir);
    }
    let project_cache = root.join(".fastembed_cache");
    if project_cache.is_dir() {
        return project_cache;
    }
    dirs::cache_dir()
        .map(|d| d.join("ragpilot").join("models"))
        .unwrap_or(project_cache)
}

/// Whether `cache_dir` holds at least one downloaded model snapshot.
pub fn cache_has_model(cache_dir: &std::path::Path) -> bool {
    std::fs::read_dir(cache_dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.file_name().to_string_lossy().starts_with("models--")
                    && e.path().is_dir()
            })
        })
        .unwrap_or(false)
}

pub struct LocalEmbedder {
    inner: Arc<TextEmbedding>,
    dimension: usize,
    #[allow(dead_code)]
    model_name: String,
}

impl LocalEmbedder {
    pub fn new(config: &LocalEmbeddingConfig, root: &std::path::Path) -> Result<Self> {
        let info = resolve_model(&config.model)?;
        let cache_dir = resolve_cache_dir(config, root);

        let opts = InitOptions::new(info.model)
            .with_show_download_progress(true)
            .with_cache_dir(cache_dir.clone());

        tracing::info!(
            "Loading local embedding model: {} (cache: {})",
            config.model,
            cache_dir.display()
        );
        let te = TextEmbedding::try_new(opts).map_err(|e| {
            anyhow!(
                "Failed to initialize local embedder: {e}\n\
                 Model cache: {}\n\
                 The first run downloads the model (~130MB) from huggingface.co.\n\
                 On an offline/air-gapped machine, copy a populated cache from a\n\
                 networked machine to that path (or point embedding.local.cache_dir\n\
                 in .rag/config.toml at one), then retry.",
                cache_dir.display()
            )
        })?;

        Ok(Self {
            inner: Arc::new(te),
            dimension: info.dimension,
            model_name: config.model.clone(),
        })
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inner = Arc::clone(&self.inner);
        let texts_owned: Vec<String> = texts.to_vec();

        let result = tokio::task::spawn_blocking(move || {
            inner.embed(texts_owned, None)
                .map_err(|e| anyhow!("Local embedding failed: {e}"))
        })
        .await
        .map_err(|e| anyhow!("spawn_blocking join error: {e}"))??;

        Ok(result)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }
}

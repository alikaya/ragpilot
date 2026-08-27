//! The brain's search/index runtime — one per process, built on first use.
//!
//! The brain is deliberately independent of any project: a `brain_*` tool must
//! work in an unregistered folder, because "talk to me anywhere" is the whole
//! point of having a brain rather than a project index. It therefore carries
//! its own embedder and vector store instead of borrowing a project's.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::OnceCell;

use super::config::BrainConfig;
use crate::orchestrator::IndexOrchestrator;
use crate::store::{ScoredChunk, SearchFilters};

pub struct BrainRuntime {
    pub config: BrainConfig,
    pub orchestrator: Arc<IndexOrchestrator>,
}

static RUNTIME: OnceCell<Arc<BrainRuntime>> = OnceCell::const_new();

/// The shared runtime, loading the embedding model on first call. Fails with an
/// actionable message when no brain has been set up yet.
pub async fn runtime() -> Result<Arc<BrainRuntime>> {
    RUNTIME
        .get_or_try_init(|| async {
            if !super::exists() {
                anyhow::bail!(
                    "No brain at {} — run `ragpilot brain init` first.",
                    super::dir().display()
                );
            }
            let config = BrainConfig::load(&super::config_path())?;
            let index_config = super::index_config()?;
            let paths = crate::paths::ProjectPaths::brain();
            let orchestrator = Arc::new(
                crate::indexer::build_orchestrator_at(&paths, &index_config)
                    .context("Cannot open the brain index")?,
            );
            Ok(Arc::new(BrainRuntime { config, orchestrator }))
        })
        .await
        .cloned()
}

impl BrainRuntime {
    /// Semantic search over the vault. `area` narrows to one part of it
    /// (`knowledge`, `daily`, `skills`).
    pub async fn search(&self, query: &str, limit: u64, area: Option<&str>) -> Result<Vec<ScoredChunk>> {
        let vectors = self.orchestrator.embedder.embed(&[query.to_string()]).await?;
        let vector = vectors
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No embedding produced"))?;

        let filters = SearchFilters {
            path_glob: area.map(|a| format!("{a}/**")),
            language: None,
            limit,
        };
        self.orchestrator.vector_store.search(&vector, filters).await
    }

    /// Index a single vault file so a note written a second ago is findable
    /// now. Cheap: one file, one embedding batch. Returns whether the file
    /// actually changed anything.
    pub async fn index_file(&self, path: &std::path::Path) -> Result<bool> {
        self.orchestrator.process_file(path).await
    }
}

/// Areas `brain_search` accepts as a filter.
pub const AREAS: &[&str] = &["knowledge", "daily", "skills"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_match_the_vault_tree() {
        let root = crate::brain::dir();
        for area in AREAS {
            let dir = root.join(area);
            assert!(dir.starts_with(&root), "{area} is not part of the vault");
        }
        // `inbox` and `archive` are deliberately not searchable: the first is
        // unprocessed input, the second is raw imported history.
        assert!(!AREAS.contains(&"inbox"));
        assert!(!AREAS.contains(&"archive"));
    }
}

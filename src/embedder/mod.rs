use anyhow::Result;
use async_trait::async_trait;

pub mod local;
pub mod api;

use crate::config::EmbeddingConfig;

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
    #[allow(dead_code)]
    fn model_name(&self) -> &str;
}

/// `root` is the project root — the local embedder anchors its model cache to
/// it so cache resolution never depends on the process working directory.
pub fn create(config: &EmbeddingConfig, root: &std::path::Path) -> Result<Box<dyn Embedder>> {
    match config.provider.as_str() {
        "local" => Ok(Box::new(local::LocalEmbedder::new(&config.local, root)?)),
        "api"   => Ok(Box::new(api::ApiEmbedder::new(&config.api)?)),
        other   => anyhow::bail!("Unknown embedding provider: '{}'. Use 'local' or 'api'.", other),
    }
}

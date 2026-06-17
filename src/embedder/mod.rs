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

pub fn create(config: &EmbeddingConfig) -> Result<Box<dyn Embedder>> {
    match config.provider.as_str() {
        "local" => Ok(Box::new(local::LocalEmbedder::new(&config.local)?)),
        "api"   => Ok(Box::new(api::ApiEmbedder::new(&config.api)?)),
        other   => anyhow::bail!("Unknown embedding provider: '{}'. Use 'local' or 'api'.", other),
    }
}

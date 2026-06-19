use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod qdrant;
pub mod sqlite;
pub mod symbol_graph;
pub mod project_tree;
pub mod impact_index;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Stable chunk ID: "relative/path:chunk_index" — used by rag_get_chunks
    pub id: String,
    pub content: String,
    pub source: String,       // relative file path
    pub chunk_index: usize,
    pub chunk_total: usize,
    pub start_line: usize,    // 1-based
    pub end_line: usize,      // 1-based
    pub file_hash: String,
    pub content_type: String, // "code", "documentation", "config"
    pub language: String,     // "rust", "python", etc.
    pub symbol: Option<String>, // first symbol definition found in this chunk
}

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: f32,
}

/// Filters for semantic search.
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Glob pattern to match against source paths, e.g. "src/**/*.rs"
    pub path_glob: Option<String>,
    /// Language name to filter on, e.g. "rust", "python"
    pub language: Option<String>,
    /// Max results to return
    pub limit: u64,
}

#[derive(Debug, Clone)]
pub struct CollectionInfo {
    pub name: String,
    #[allow(dead_code)]
    pub vectors_count: u64,
    pub points_count: u64,
    pub dimension: u64,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn ensure_collection(&self, dim: u64) -> Result<()>;
    async fn upsert_chunks(&self, chunks: &[Chunk], vectors: &[Vec<f32>]) -> Result<()>;
    async fn search(&self, vector: &[f32], filters: SearchFilters) -> Result<Vec<ScoredChunk>>;
    /// Fetch chunks by their stable IDs (payload `id` field).
    async fn get_chunks_by_ids(&self, ids: &[&str]) -> Result<Vec<Chunk>>;
    async fn delete_by_source(&self, source: &str) -> Result<()>;
    async fn collection_info(&self) -> Result<CollectionInfo>;
    async fn delete_collection(&self) -> Result<()>;
}

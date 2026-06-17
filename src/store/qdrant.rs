use anyhow::{anyhow, Result};
use async_trait::async_trait;
use qdrant_client::{
    Payload, Qdrant,
    qdrant::{
        Condition, CreateCollectionBuilder, DeletePointsBuilder, Distance, Filter,
        PointStruct, QueryPointsBuilder, ScrollPointsBuilder, UpsertPointsBuilder,
        VectorParamsBuilder, vectors_config::Config as VectorsConfigInner,
    },
};
use serde_json::json;
use uuid::Uuid;

use crate::config::QdrantConfig;
use super::{Chunk, CollectionInfo, ScoredChunk, SearchFilters, VectorStore};

pub struct QdrantStore {
    client: Qdrant,
    collection: String,
}

impl QdrantStore {
    pub fn new(config: &QdrantConfig) -> Result<Self> {
        let mut builder = Qdrant::from_url(&config.url);
        if let Some(ref key) = config.api_key {
            builder = builder.api_key(key.clone());
        }
        let client = builder.build()
            .map_err(|e| anyhow!("Failed to connect to Qdrant: {e}"))?;
        let collection = config.collection
            .clone()
            .unwrap_or_else(|| "rag_default".to_string());
        Ok(Self { client, collection })
    }

    fn payload_to_chunk(payload: &std::collections::HashMap<String, qdrant_client::qdrant::Value>) -> Chunk {
        let get_str = |k: &str| -> String {
            payload.get(k)
                .and_then(|v| v.as_str())
                .cloned()
                .unwrap_or_default()
        };
        let get_i64 = |k: &str| -> usize {
            payload.get(k)
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as usize
        };

        Chunk {
            id:           get_str("id"),
            content:      get_str("content"),
            source:       get_str("source"),
            chunk_index:  get_i64("chunk_index"),
            chunk_total:  get_i64("chunk_total"),
            start_line:   get_i64("start_line"),
            end_line:     get_i64("end_line"),
            file_hash:    get_str("file_hash"),
            content_type: get_str("content_type"),
            language:     get_str("language"),
            symbol: {
                let s = get_str("symbol");
                if s.is_empty() { None } else { Some(s) }
            },
        }
    }
}

#[async_trait]
impl VectorStore for QdrantStore {
    async fn ensure_collection(&self, dim: u64) -> Result<()> {
        match self.client.collection_info(&self.collection).await {
            Ok(_) => {
                tracing::debug!("Collection '{}' already exists", self.collection);
                return Ok(());
            }
            Err(_) => {}
        }

        tracing::info!("Creating collection '{}' with dim={}", self.collection, dim);
        self.client.create_collection(
            CreateCollectionBuilder::new(&self.collection)
                .vectors_config(VectorParamsBuilder::new(dim, Distance::Cosine))
        )
        .await
        .map_err(|e| anyhow!("Failed to create collection: {e}"))?;

        Ok(())
    }

    async fn upsert_chunks(&self, chunks: &[Chunk], vectors: &[Vec<f32>]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let points: Vec<PointStruct> = chunks.iter().zip(vectors.iter()).map(|(chunk, vec)| {
            let point_id = Uuid::new_v4().to_string();
            let payload = Payload::try_from(json!({
                "id":           chunk.id,
                "content":      chunk.content,
                "source":       chunk.source,
                "chunk_index":  chunk.chunk_index as i64,
                "chunk_total":  chunk.chunk_total as i64,
                "start_line":   chunk.start_line as i64,
                "end_line":     chunk.end_line as i64,
                "file_hash":    chunk.file_hash,
                "content_type": chunk.content_type,
                "language":     chunk.language,
                "symbol":       chunk.symbol.as_deref().unwrap_or(""),
            }))
            .unwrap_or_default();

            PointStruct::new(point_id, vec.clone(), payload)
        }).collect();

        self.client.upsert_points(
            UpsertPointsBuilder::new(&self.collection, points)
        )
        .await
        .map_err(|e| anyhow!("Failed to upsert chunks: {e}"))?;

        Ok(())
    }

    async fn search(&self, vector: &[f32], filters: SearchFilters) -> Result<Vec<ScoredChunk>> {
        // Build Qdrant-side filters (language only — path glob is post-filtered in Rust)
        let mut conditions: Vec<Condition> = Vec::new();
        if let Some(ref lang) = filters.language {
            conditions.push(Condition::matches("language", lang.clone()));
        }

        let limit = if filters.limit > 0 { filters.limit } else { 6 };
        // Fetch more than needed when path glob filtering will be applied
        let fetch_limit = if filters.path_glob.is_some() { limit * 4 } else { limit };

        let mut builder = QueryPointsBuilder::new(&self.collection)
            .query(vector.to_vec())
            .limit(fetch_limit)
            .with_payload(true);

        if !conditions.is_empty() {
            builder = builder.filter(Filter::must(conditions));
        }

        let response = self.client.query(builder)
            .await
            .map_err(|e| anyhow!("Search failed: {e}"))?;

        let mut results: Vec<ScoredChunk> = response.result.iter().map(|scored| {
            ScoredChunk {
                chunk: Self::payload_to_chunk(&scored.payload),
                score: scored.score,
            }
        }).collect();

        // Post-filter by path glob if requested
        if let Some(ref pattern_str) = filters.path_glob {
            if let Ok(pattern) = glob::Pattern::new(pattern_str) {
                results.retain(|r| {
                    pattern.matches_with(
                        &r.chunk.source,
                        glob::MatchOptions { case_sensitive: false, ..Default::default() },
                    )
                });
            }
        }

        results.truncate(limit as usize);
        Ok(results)
    }

    async fn get_chunks_by_ids(&self, ids: &[&str]) -> Result<Vec<Chunk>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let conditions: Vec<Condition> = ids.iter()
            .map(|id| Condition::matches("id", id.to_string()))
            .collect();

        let response = self.client.scroll(
            ScrollPointsBuilder::new(&self.collection)
                .filter(Filter::should(conditions))
                .with_payload(true)
                .limit(ids.len() as u32)
        )
        .await
        .map_err(|e| anyhow!("Scroll failed: {e}"))?;

        Ok(response.result.iter()
            .map(|p| Self::payload_to_chunk(&p.payload))
            .collect())
    }

    async fn delete_by_source(&self, source: &str) -> Result<()> {
        self.client.delete_points(
            DeletePointsBuilder::new(&self.collection)
                .points(Filter::must([Condition::matches("source", source.to_string())]))
        )
        .await
        .map_err(|e| anyhow!("Failed to delete points for source '{}': {e}", source))?;

        Ok(())
    }

    async fn collection_info(&self) -> Result<CollectionInfo> {
        let info = self.client.collection_info(&self.collection)
            .await
            .map_err(|e| anyhow!("Failed to get collection info: {e}"))?;

        let result = info.result.ok_or_else(|| anyhow!("No collection info returned"))?;
        let points_count = result.points_count.unwrap_or(0);
        let vectors_count = result.indexed_vectors_count.unwrap_or(0);

        let dimension = result.config
            .and_then(|c| c.params)
            .and_then(|p| p.vectors_config)
            .and_then(|vc| vc.config)
            .and_then(|c| match c {
                VectorsConfigInner::Params(vp) => Some(vp.size),
                _ => None,
            })
            .unwrap_or(0);

        Ok(CollectionInfo {
            name: self.collection.clone(),
            vectors_count,
            points_count,
            dimension,
        })
    }

    async fn delete_collection(&self) -> Result<()> {
        self.client.delete_collection(&self.collection)
            .await
            .map_err(|e| anyhow!("Failed to delete collection: {e}"))?;
        Ok(())
    }
}

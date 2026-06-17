use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::ApiEmbeddingConfig;
use super::Embedder;

#[derive(Debug, Clone, PartialEq)]
enum Provider {
    OpenAI,
    Cohere,
    Jina,
}

pub struct ApiEmbedder {
    client: Client,
    provider: Provider,
    model: String,
    api_key: String,
    batch_size: usize,
    dimension: usize,
}

// OpenAI / Jina response
#[derive(Deserialize)]
struct OpenAIEmbeddingData {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct OpenAIEmbeddingResponse {
    data: Vec<OpenAIEmbeddingData>,
}

// Cohere response
#[derive(Deserialize)]
struct CohereEmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Serialize)]
struct OpenAIRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Serialize)]
struct CohereRequest<'a> {
    model: &'a str,
    texts: &'a [String],
    input_type: &'a str,
}

impl ApiEmbedder {
    pub fn new(config: &ApiEmbeddingConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env)
            .map_err(|_| anyhow!(
                "Environment variable '{}' not set (required for {} embedding)",
                config.api_key_env, config.provider
            ))?;

        let (provider, dimension) = match config.provider.as_str() {
            "openai" => (Provider::OpenAI, openai_model_dimension(&config.model)),
            "cohere" => (Provider::Cohere, cohere_model_dimension(&config.model)),
            "jina"   => (Provider::Jina, 768),
            other    => anyhow::bail!("Unknown API embedding provider: '{}'", other),
        };

        Ok(Self {
            client: Client::new(),
            provider,
            model: config.model.clone(),
            api_key,
            batch_size: config.batch_size,
            dimension,
        })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match self.provider {
            Provider::OpenAI => {
                let resp: OpenAIEmbeddingResponse = self.client
                    .post("https://api.openai.com/v1/embeddings")
                    .bearer_auth(&self.api_key)
                    .json(&OpenAIRequest { model: &self.model, input: texts })
                    .send().await?
                    .error_for_status()?
                    .json().await?;
                Ok(resp.data.into_iter().map(|d| d.embedding).collect())
            }
            Provider::Cohere => {
                let resp: CohereEmbeddingResponse = self.client
                    .post("https://api.cohere.ai/v1/embed")
                    .bearer_auth(&self.api_key)
                    .json(&CohereRequest {
                        model: &self.model,
                        texts,
                        input_type: "search_document",
                    })
                    .send().await?
                    .error_for_status()?
                    .json().await?;
                Ok(resp.embeddings)
            }
            Provider::Jina => {
                let resp: OpenAIEmbeddingResponse = self.client
                    .post("https://api.jina.ai/v1/embeddings")
                    .bearer_auth(&self.api_key)
                    .json(&OpenAIRequest { model: &self.model, input: texts })
                    .send().await?
                    .error_for_status()?
                    .json().await?;
                Ok(resp.data.into_iter().map(|d| d.embedding).collect())
            }
        }
    }
}

fn openai_model_dimension(model: &str) -> usize {
    match model {
        "text-embedding-3-large" => 3072,
        "text-embedding-3-small" | "text-embedding-ada-002" => 1536,
        _ => 1536,
    }
}

fn cohere_model_dimension(model: &str) -> usize {
    match model {
        "embed-english-light-v3.0" | "embed-multilingual-light-v3.0" => 384,
        _ => 1024,
    }
}

#[async_trait]
impl Embedder for ApiEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.batch_size) {
            let mut batch = self.embed_batch(chunk).await?;
            all.append(&mut batch);
        }
        Ok(all)
    }

    fn dimension(&self) -> usize { self.dimension }
    fn model_name(&self) -> &str { &self.model }
}

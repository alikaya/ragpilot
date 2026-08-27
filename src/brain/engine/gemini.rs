//! Compile through the Gemini API.
//!
//! The key comes from `GEMINI_API_KEY` at call time and is never written to
//! `brain/config.toml` — a config file gets committed, shared and pasted into
//! issues; an environment variable does not.

use async_trait::async_trait;
use serde_json::json;

use super::{CompileRequest, CompilerEngine, EngineError};

pub const API_KEY_ENV: &str = "GEMINI_API_KEY";
const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

pub struct GeminiApiEngine {
    model: String,
    base_url: String,
}

impl GeminiApiEngine {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), base_url: DEFAULT_BASE.to_string() }
    }

    /// Point at a different host — tests only.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into();
        self
    }

    fn api_key() -> Result<String, EngineError> {
        std::env::var(API_KEY_ENV).ok().filter(|k| !k.is_empty()).ok_or_else(|| {
            EngineError::Unavailable(format!(
                "{API_KEY_ENV} is not set. Export it, or switch the brain to the \
                 claude-cli engine (which uses your subscription instead)."
            ))
        })
    }

    /// The request body, split out so a test can assert its shape without a
    /// network round trip.
    fn body(req: &CompileRequest<'_>) -> serde_json::Value {
        json!({
            "system_instruction": { "parts": [{ "text": req.system }] },
            "contents": [{ "role": "user", "parts": [{ "text": req.input }] }],
            "generationConfig": { "maxOutputTokens": req.max_output_tokens }
        })
    }

    /// Pull the text out of a Gemini response, or explain what came back
    /// instead — a blocked or truncated answer must not look like an empty note.
    fn extract_text(payload: &serde_json::Value) -> Result<String, EngineError> {
        if let Some(message) = payload.pointer("/error/message").and_then(|v| v.as_str()) {
            return Err(EngineError::Failed(format!("Gemini API error: {message}")));
        }
        let candidate = payload
            .pointer("/candidates/0")
            .ok_or_else(|| EngineError::Failed("Gemini returned no candidates".into()))?;

        let text: String = candidate
            .pointer("/content/parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        if text.trim().is_empty() {
            let reason = candidate
                .get("finishReason")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown");
            return Err(EngineError::Failed(format!(
                "Gemini returned no text (finishReason: {reason})"
            )));
        }
        Ok(text.trim().to_string())
    }
}

#[async_trait]
impl CompilerEngine for GeminiApiEngine {
    fn name(&self) -> &'static str { "gemini-api" }

    fn available(&self) -> Result<(), EngineError> {
        Self::api_key().map(|_| ())
    }

    async fn complete(&self, req: CompileRequest<'_>) -> Result<String, EngineError> {
        let key = Self::api_key()?;
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            self.model
        );

        let response = reqwest::Client::new()
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&Self::body(&req))
            .send()
            .await
            .map_err(|e| EngineError::Failed(format!("Gemini request failed: {e}")))?;

        let status = response.status();
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|e| EngineError::Failed(format!("Gemini returned invalid JSON: {e}")))?;

        if !status.is_success() {
            let message = payload
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("no message");
            return Err(EngineError::Failed(format!("Gemini HTTP {status}: {message}")));
        }
        Self::extract_text(&payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_carries_system_input_and_budget() {
        let req = CompileRequest { system: "be terse", input: "raw log", max_output_tokens: 512 };
        let body = GeminiApiEngine::body(&req);

        assert_eq!(body.pointer("/system_instruction/parts/0/text").unwrap(), "be terse");
        assert_eq!(body.pointer("/contents/0/parts/0/text").unwrap(), "raw log");
        assert_eq!(body.pointer("/generationConfig/maxOutputTokens").unwrap(), 512);
        // The key travels in a header, never in the body.
        assert!(!body.to_string().contains("api_key"));
    }

    #[test]
    fn extract_text_joins_parts() {
        let payload = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "one " }, { "text": "two" }] } }]
        });
        assert_eq!(GeminiApiEngine::extract_text(&payload).unwrap(), "one two");
    }

    #[test]
    fn extract_text_reports_why_it_is_empty() {
        let blocked = serde_json::json!({
            "candidates": [{ "finishReason": "SAFETY", "content": { "parts": [] } }]
        });
        let err = GeminiApiEngine::extract_text(&blocked).unwrap_err().to_string();
        assert!(err.contains("SAFETY"), "{err}");

        let api_error = serde_json::json!({ "error": { "message": "quota exceeded" } });
        let err = GeminiApiEngine::extract_text(&api_error).unwrap_err().to_string();
        assert!(err.contains("quota exceeded"), "{err}");

        let empty = serde_json::json!({ "candidates": [] });
        assert!(GeminiApiEngine::extract_text(&empty).is_err());
    }
}

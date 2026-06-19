use serde_json::json;

use super::McpContext;
use crate::config::Config;
use crate::indexer::IndexState;
use crate::mcp::protocol::{McpRequest, McpResponse};

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "rag_index_status",
            "description": "Returns RAG index statistics: files indexed, chunks, last commit, dirty file count.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "rag_ensure_index",
            "description": "Incrementally re-indexes all changed files. Call this to make sure the index is current before starting a task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "force": { "type": "boolean", "description": "Force full reindex (default false)", "default": false }
                }
            }
        }),
    ]
}

// ─── rag_index_status ────────────────────────────────────────────────────────

pub async fn status(req: &McpRequest, ctx: &McpContext) -> McpResponse {
    let state_path = Config::state_path(&ctx.root);
    let state      = IndexState::load(&state_path).unwrap_or_default();

    let git_commit = std::process::Command::new("git")
        .args(["-C", &ctx.root.to_string_lossy().into_owned(), "rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
        } else { None })
        .unwrap_or_else(|| "unknown".to_string());

    // Count dirty files
    let dirty = state.file_hashes.iter().filter(|(rel, stored_hash)| {
        match std::fs::read_to_string(ctx.root.join(rel.as_str())) {
            Ok(c)  => &crate::indexer::compute_hash(&c) != *stored_hash,
            Err(_) => true,
        }
    }).count();

    let indexed_at = state.indexed_at
        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "never".to_string());

    let text = format!(
        "Project:       {}\n\
         Collection:    {}\n\
         Files indexed: {}\n\
         Chunks:        ~{}\n\
         Model:         {} ({})\n\
         Last indexed:  {}\n\
         Git commit:    {}\n\
         Dirty files:   {}",
        ctx.config.project.name,
        ctx.config.qdrant.collection_name(&ctx.config.project.name),
        state.total_files,
        state.total_chunks,
        state.embedding_model,
        state.embedding_provider,
        indexed_at,
        git_commit,
        dirty,
    );

    McpResponse::tool_text(req.id.clone(), text)
}

// ─── rag_ensure_index ────────────────────────────────────────────────────────

pub async fn ensure(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    tracing::info!("rag_ensure_index: force={}", force);

    match ctx.orchestrator.ensure_index(force).await {
        Ok(r) => {
            let result = json!({
                "dirty_count": r.dirty_count,
                "indexed":     r.indexed,
                "duration_ms": r.duration_ms,
                "message":     format!("Indexed {} of {} dirty files in {}ms", r.indexed, r.dirty_count, r.duration_ms),
            });
            McpResponse::tool_text(
                req.id.clone(),
                serde_json::to_string_pretty(&result).unwrap_or_default(),
            )
        }
        Err(e) => McpResponse::tool_error(req.id.clone(), format!("ensure_index error: {e}")),
    }
}

use std::sync::Arc;

use crate::config::Config;
use crate::embedder::Embedder;
use crate::orchestrator::IndexOrchestrator;
use crate::store::impact_index::ImpactIndexStore;
use crate::store::project_tree::ProjectTreeStore;
use crate::store::symbol_graph::SymbolGraphStore;
use crate::store::VectorStore;
use super::protocol::{McpRequest, McpResponse};

pub mod rag;
pub mod nav;
pub mod impact;
pub mod context;
pub mod index;
pub mod review;

// ─── Context ─────────────────────────────────────────────────────────────────

pub struct McpContext {
    pub config:       Arc<Config>,
    pub root:         std::path::PathBuf,
    pub embedder:     Arc<dyn Embedder>,
    pub store:        Arc<dyn VectorStore>,
    pub symbol_graph: Arc<SymbolGraphStore>,
    pub project_tree: Arc<ProjectTreeStore>,
    pub impact_index: Arc<ImpactIndexStore>,
    pub orchestrator: Arc<IndexOrchestrator>,
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

pub async fn handle_request(req: &McpRequest, ctx: &Arc<McpContext>) -> McpResponse {
    match req.method.as_str() {
        "initialize"  => handle_initialize(req),
        "tools/list"  => handle_tools_list(req, ctx),
        "tools/call"  => handle_tools_call(req, ctx).await,
        other => McpResponse::error(-32601, &format!("Method not found: {other}"), req.id.clone()),
    }
}

fn handle_initialize(req: &McpRequest) -> McpResponse {
    // Echo the client's requested protocol version when provided so strict newer
    // clients (e.g. Antigravity CLI / Gemini) negotiate cleanly; fall back to a
    // known-good version otherwise. The tools capability shape is stable across
    // these revisions, so echoing is safe.
    let version = req
        .params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or("2024-11-05")
        .to_string();
    McpResponse::ok(req.id.clone(), serde_json::json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ragpilot", "version": env!("CARGO_PKG_VERSION") }
    }))
}

fn handle_tools_list(req: &McpRequest, ctx: &McpContext) -> McpResponse {
    let mut tools = Vec::new();
    tools.extend(rag::tool_definitions(ctx));
    tools.extend(nav::tool_definitions());
    tools.extend(impact::tool_definitions());
    tools.extend(context::tool_definitions());
    tools.extend(index::tool_definitions());
    tools.extend(review::tool_definitions());
    McpResponse::ok(req.id.clone(), serde_json::json!({ "tools": tools }))
}

async fn handle_tools_call(req: &McpRequest, ctx: &Arc<McpContext>) -> McpResponse {
    let params = match req.params.as_ref() {
        Some(p) => p,
        None    => return McpResponse::error(-32602, "Missing params", req.id.clone()),
    };
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None    => return McpResponse::error(-32602, "Missing tool name", req.id.clone()),
    };
    let args = params.get("arguments").unwrap_or(&serde_json::Value::Null);

    // Tool names use underscores (e.g. `rag_search`) because several MCP clients
    // (Antigravity/Gemini, Copilot, Cursor) reject or silently drop names with
    // dots. Normalize any legacy dotted name to its underscore form so older
    // configs keep working.
    let normalized = name.replace('.', "_");

    match normalized.as_str() {
        // RAG tools
        "rag_search"           => rag::search(req, args, ctx).await,
        "rag_get_chunks"       => rag::get_chunks(req, args, ctx).await,
        "rag_get_file_ranges"  => rag::get_file_ranges(req, args, ctx),
        "rag_get_skeleton"     => rag::get_skeleton(req, args, ctx),
        // Navigation
        "nav_symbol_resolve"   => nav::symbol_resolve(req, args, ctx).await,
        "nav_call_graph"       => nav::call_graph(req, args, ctx).await,
        // Impact
        "impact_analyze"       => impact::analyze(req, args, ctx).await,
        // Review / semantic diff
        "review_semantic_diff" => review::semantic_diff(req, args, ctx).await,
        // Context bundle
        "context_bundle"       => context::bundle(req, args, ctx).await,
        // Index management
        "rag_index_status"     => index::status(req, ctx).await,
        "rag_ensure_index"     => index::ensure(req, args, ctx).await,
        other => McpResponse::tool_error(req.id.clone(), format!("Unknown tool: {other}")),
    }
}

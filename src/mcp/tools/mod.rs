use std::sync::Arc;

use crate::config::Config;
use crate::embedder::Embedder;
use crate::orchestrator::IndexOrchestrator;
use crate::store::impact_index::ImpactIndexStore;
use crate::store::project_tree::ProjectTreeStore;
use crate::store::symbol_graph::SymbolGraphStore;
use crate::store::VectorStore;
use super::protocol::{McpRequest, McpResponse};

pub mod brain;
pub mod rag;
pub mod nav;
pub mod impact;
pub mod context;
pub mod index;
pub mod review;

// ─── Path safety ─────────────────────────────────────────────────────────────

/// Resolve a client-supplied relative path against the project root, refusing
/// anything that escapes it. A leading `/` is treated as project-relative
/// (`/src/x` → `<root>/src/x`), but `..` components and OS-absolute/prefixed
/// paths are rejected outright — this alone stops `../../../etc/passwd`-style
/// traversal even for not-yet-existing files. When both the target and the
/// root canonicalize (the file exists), a containment check on the canonical
/// paths additionally defeats symlink escapes.
pub fn resolve_in_root(root: &std::path::Path, rel_input: &str) -> Result<std::path::PathBuf, String> {
    use std::path::Component;

    let rel = std::path::Path::new(rel_input.trim_start_matches('/'));
    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err("path escapes project root ('..' is not allowed)".into()),
            Component::RootDir | Component::Prefix(_) => return Err("absolute paths are not allowed".into()),
        }
    }

    let joined = root.join(rel);
    match (root.canonicalize(), joined.canonicalize()) {
        (Ok(canon_root), Ok(canon_target)) => {
            if canon_target.starts_with(&canon_root) {
                Ok(canon_target)
            } else {
                Err("path escapes project root".into())
            }
        }
        // Target does not exist yet (or root is not canonicalizable): the
        // component check above already guarantees containment lexically.
        _ => Ok(joined),
    }
}

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

/// Guidance returned when a tool is called before any project is loaded — e.g.
/// a global (folder-independent) client that launched the server without a
/// project directory. `initialize` and `tools/list` still succeed so the
/// handshake never fails; only tool calls need a loaded project.
const NO_PROJECT_MSG: &str = "ragpilot: no project is loaded. Launch the server with \
`--root <path>`, set the RAGPILOT_ROOT environment variable, or open a registered \
project folder (run `ragpilot init .` there first).";

/// Filled in at startup with the resolution-specific reason a project could not
/// be loaded — "not registered", "looks like it moved, run `projects relink`",
/// and so on. Falls back to [`NO_PROJECT_MSG`] when the server never got far
/// enough to resolve a folder.
static NO_PROJECT_HINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record why no project is loaded. First call wins; later ones are ignored so
/// the original startup diagnosis is what the agent eventually sees.
pub fn set_no_project_hint(message: String) {
    let _ = NO_PROJECT_HINT.set(message);
}

fn no_project_message() -> String {
    NO_PROJECT_HINT
        .get()
        .cloned()
        .unwrap_or_else(|| NO_PROJECT_MSG.to_string())
}

const DEFAULT_SEARCH_DESC: &str = "Semantic search over this project's code and docs. Returns \
path, line range and a snippet — read the ranges you need with rag_get_file_ranges.";

/// Whether a request is a `brain_*` tool call.
///
/// Used to keep brain traffic out of the observation seam: the brain holds the
/// user's own content — notes, decisions, personal context — and a build that
/// supplies an observer must never see it. Excluding it here makes that
/// structural rather than a rule an observer is asked to follow.
pub fn is_brain_request(req: &McpRequest) -> bool {
    if req.method != "tools/call" {
        return false;
    }
    let Some(name) = req
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    else {
        return false;
    };
    let normalized = name.replace('.', "_");
    brain::TOOL_NAMES.contains(&normalized.as_str())
}

pub async fn handle_request(req: &McpRequest, ctx: Option<&Arc<McpContext>>) -> McpResponse {
    match req.method.as_str() {
        // Handshake methods never depend on a loaded project, so they always
        // answer cleanly — this is what prevents the client-visible "EOF".
        "initialize"  => handle_initialize(req),
        "tools/list"  => handle_tools_list(req, ctx.map(|c| &**c)),
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

fn handle_tools_list(req: &McpRequest, ctx: Option<&McpContext>) -> McpResponse {
    let search_desc = ctx
        .map(|c| c.config.mcp.search_tool_description.as_str())
        .unwrap_or(DEFAULT_SEARCH_DESC);
    let mut tools = Vec::new();
    tools.extend(rag::tool_definitions(search_desc));
    tools.extend(nav::tool_definitions());
    tools.extend(impact::tool_definitions());
    tools.extend(context::tool_definitions());
    tools.extend(index::tool_definitions());
    tools.extend(review::tool_definitions());
    // Brain tools are always listed: they need no project.
    tools.extend(brain::tool_definitions());
    McpResponse::ok(req.id.clone(), serde_json::json!({ "tools": tools }))
}

async fn handle_tools_call(req: &McpRequest, ctx: Option<&Arc<McpContext>>) -> McpResponse {
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

    // The brain belongs to the machine, not to a project, so its tools answer
    // in an unregistered folder too — that is the whole point of having one.
    if brain::TOOL_NAMES.contains(&normalized.as_str()) {
        return brain::handle(&normalized, req, args).await;
    }

    let ctx = match ctx {
        Some(c) => c,
        None => return McpResponse::tool_error(req.id.clone(), no_project_message()),
    };

    match normalized.as_str() {
        // RAG tools
        "rag_search"           => rag::search(req, args, ctx).await,
        "rag_get_chunks"       => rag::get_chunks(req, args, ctx).await,
        "rag_get_file_ranges"  => rag::get_file_ranges(req, args, ctx).await,
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

#[cfg(test)]
mod path_safety_tests {
    use super::resolve_in_root;
    use std::path::Path;

    #[test]
    fn rejects_parent_dir_traversal() {
        let root = Path::new("/home/user/project");
        assert!(resolve_in_root(root, "../../../../etc/passwd").is_err());
        assert!(resolve_in_root(root, "src/../../etc/passwd").is_err());
        assert!(resolve_in_root(root, "..").is_err());
    }

    #[test]
    fn leading_slash_is_project_relative_not_absolute() {
        let root = Path::new("/home/user/project");
        // A leading slash is stripped and treated as project-relative; the
        // result stays under root (file need not exist for this check).
        let p = resolve_in_root(root, "/etc/passwd").unwrap();
        assert!(p.starts_with(root));
        assert!(p.ends_with("etc/passwd"));
    }

    #[test]
    fn plain_relative_paths_pass() {
        let root = Path::new("/home/user/project");
        let p = resolve_in_root(root, "src/main.rs").unwrap();
        assert_eq!(p, Path::new("/home/user/project/src/main.rs"));
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str) -> McpRequest {
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": {} }
        }))
        .unwrap()
    }

    #[test]
    fn brain_traffic_is_kept_out_of_the_observation_seam() {
        // Every brain tool, including the dotted legacy spelling.
        for name in brain::TOOL_NAMES {
            assert!(is_brain_request(&call(name)), "{name} would be observed");
        }
        assert!(is_brain_request(&call("brain.note")));

        // Project tools are observable, as they always were.
        for name in ["rag_search", "context_bundle", "nav_call_graph", "rag_index_status"] {
            assert!(!is_brain_request(&call(name)), "{name} should stay observable");
        }

        // Non-tool traffic is not a brain call.
        let initialize: McpRequest =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).unwrap();
        assert!(!is_brain_request(&initialize));
    }
}

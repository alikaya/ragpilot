//! `brain_*` MCP tools.
//!
//! These are the only tools that work without a loaded project: the brain
//! belongs to the machine, not to a repo, so an agent in an unregistered
//! folder can still remember who it is and what was decided yesterday.

use serde_json::json;

use crate::brain::{self, runtime, vault};
use crate::mcp::protocol::{McpRequest, McpResponse};

/// Every tool name handled here.
pub const TOOL_NAMES: &[&str] = &["brain_load", "brain_search", "brain_note", "brain_flush"];

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "brain_load",
            "description": "Session-opening context from the second brain: persona, rules, open threads, \
recent decisions. Call first in a session. Works in any folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_tokens": {
                        "type": "integer",
                        "description": "Token budget (default: from brain config)"
                    }
                }
            }
        }),
        json!({
            "name": "brain_search",
            "description": "Semantic search over the second brain: knowledge, daily logs, skills. Use it \
when the user refers to something decided or learned earlier.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "description": "Results count (default 6)", "default": 6 },
                    "filter": {
                        "type": "string",
                        "description": "knowledge | daily | skills",
                        "enum": ["knowledge", "daily", "skills"]
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "brain_note",
            "description": "Record one thing worth keeping, the moment it happens. `kind: \"rule\"` stores \
a correction you were given (with `why`) and loads it every session after.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "kind": {
                        "type": "string",
                        "description": "note | decision | task | receipt | rule (default: note)",
                        "enum": ["note", "decision", "task", "receipt", "rule"],
                        "default": "note"
                    },
                    "why": {
                        "type": "string",
                        "description": "For kind \"rule\": why. A rule without its reason gets misapplied."
                    }
                },
                "required": ["text"]
            }
        }),
        json!({
            "name": "brain_flush",
            "description": "Close the session: store your summary, the decisions, what is still open and \
what you finished. You write it; this only stores it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary":      { "type": "string", "description": "What happened" },
                    "decisions":    { "type": "array", "items": { "type": "string" }, "description": "Decisions" },
                    "open_threads": { "type": "array", "items": { "type": "string" }, "description": "Still half-done; kept until closed" },
                    "closed_threads": { "type": "array", "items": { "type": "string" }, "description": "Threads now finished" }
                },
                "required": ["summary"]
            }
        }),
    ]
}

/// Dispatch a `brain_*` tool. Called before the project-context check, so it
/// never depends on a loaded project.
pub async fn handle(name: &str, req: &McpRequest, args: &serde_json::Value) -> McpResponse {
    match name {
        "brain_load"   => load(req, args).await,
        "brain_search" => search(req, args).await,
        "brain_note"   => note(req, args).await,
        "brain_flush"  => flush(req, args).await,
        other => McpResponse::tool_error(req.id.clone(), format!("Unknown brain tool: {other}")),
    }
}

// ── brain_load ─────────────────────────────────────────────────────────────

async fn load(req: &McpRequest, args: &serde_json::Value) -> McpResponse {
    let rt = match runtime::runtime().await {
        Ok(rt) => rt,
        Err(e) => return McpResponse::tool_error(req.id.clone(), e.to_string()),
    };

    let budget = args
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(rt.config.load.max_tokens)
        .max(1);

    match vault::load_package(budget) {
        Ok(text) => McpResponse::tool_text(req.id.clone(), text),
        Err(e) => McpResponse::tool_error(req.id.clone(), format!("brain_load error: {e}")),
    }
}

// ── brain_search ───────────────────────────────────────────────────────────

async fn search(req: &McpRequest, args: &serde_json::Value) -> McpResponse {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return McpResponse::tool_error(req.id.clone(), "Missing 'query'".into()),
    };
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(6).clamp(1, 50);

    let area = match args.get("filter").and_then(|v| v.as_str()) {
        None => None,
        Some(a) if runtime::AREAS.contains(&a) => Some(a),
        Some(bad) => {
            return McpResponse::tool_error(
                req.id.clone(),
                format!("Unknown filter '{bad}'. Use one of: {}", runtime::AREAS.join(", ")),
            )
        }
    };

    let rt = match runtime::runtime().await {
        Ok(rt) => rt,
        Err(e) => return McpResponse::tool_error(req.id.clone(), e.to_string()),
    };

    match rt.search(query, limit, area).await {
        Ok(hits) if hits.is_empty() => {
            McpResponse::tool_text(req.id.clone(), "Nothing found in the brain.".into())
        }
        Ok(hits) => {
            let items: Vec<serde_json::Value> = hits.iter().map(format_hit).collect();
            McpResponse::tool_text(
                req.id.clone(),
                serde_json::to_string_pretty(&items).unwrap_or_default(),
            )
        }
        Err(e) => McpResponse::tool_error(req.id.clone(), format!("brain_search error: {e}")),
    }
}

/// Brain hits are shown by file, heading and score — the vault is prose, so a
/// snippet plus its source is what an agent needs to decide whether to read on.
pub(crate) fn format_hit(hit: &crate::store::ScoredChunk) -> serde_json::Value {
    json!({
        "path":    hit.chunk.source,
        "heading": heading_of(&hit.chunk.content),
        "score":   (hit.score * 1000.0).round() / 1000.0,
        "snippet": snippet(&hit.chunk.content, 400),
        "chunk_id": hit.chunk.id,
    })
}

fn heading_of(content: &str) -> String {
    content
        .lines()
        .find(|l| l.trim_start().starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

fn snippet(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max_chars).collect();
    format!("{cut}…")
}

// ── brain_note ─────────────────────────────────────────────────────────────

/// Refuse to write into a vault that was never set up. Without this a note
/// lands in a directory with no config, no git and no persona — a half-made
/// brain that `brain_search` then correctly reports as missing.
fn require_brain(req: &McpRequest) -> Option<McpResponse> {
    brain::exists().then_some(()).map_or_else(
        || {
            Some(McpResponse::tool_error(
                req.id.clone(),
                format!(
                    "No brain at {} — run `ragpilot brain init` first.",
                    brain::dir().display()
                ),
            ))
        },
        |_| None,
    )
}

async fn note(req: &McpRequest, args: &serde_json::Value) -> McpResponse {
    if let Some(refusal) = require_brain(req) {
        return refusal;
    }
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return McpResponse::tool_error(req.id.clone(), "Missing 'text'".into()),
    };
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("note");

    // A rule is not an event in a log — it is a standing instruction, so it
    // lives in its own file and is loaded at the start of every session.
    let path = if kind == "rule" {
        let why = args.get("why").and_then(|v| v.as_str());
        match vault::append_rule(text, why) {
            Ok(p) => p,
            Err(e) => return McpResponse::tool_error(req.id.clone(), format!("brain_note error: {e}")),
        }
    } else {
        match vault::append_note(kind, text) {
            Ok(p) => p,
            Err(e) => return McpResponse::tool_error(req.id.clone(), format!("brain_note error: {e}")),
        }
    };

    let indexed = reindex(&path).await;
    McpResponse::tool_text(
        req.id.clone(),
        format!("Noted [{kind}] in {}{indexed}", relative(&path)),
    )
}

// ── brain_flush ────────────────────────────────────────────────────────────

async fn flush(req: &McpRequest, args: &serde_json::Value) -> McpResponse {
    if let Some(refusal) = require_brain(req) {
        return refusal;
    }
    let summary = match args.get("summary").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return McpResponse::tool_error(req.id.clone(), "Missing 'summary'".into()),
    };
    let decisions = string_list(args.get("decisions"));
    let open_threads = string_list(args.get("open_threads"));
    let closed_threads = string_list(args.get("closed_threads"));

    let path = match vault::append_flush(summary, &decisions, &open_threads) {
        Ok(p) => p,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("brain_flush error: {e}")),
    };
    if let Err(e) = vault::update_threads(&open_threads, &closed_threads) {
        return McpResponse::tool_error(req.id.clone(), format!("brain_flush error: {e}"));
    }

    let indexed = reindex(&path).await;
    McpResponse::tool_text(
        req.id.clone(),
        format!(
            "Session written to {} ({} decision(s), {} open, {} closed){indexed}",
            relative(&path),
            decisions.len(),
            open_threads.len(),
            closed_threads.len()
        ),
    )
}

fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Index a just-written file. A failure here is reported inline rather than
/// failing the call: the note is already safe on disk, which is the part that
/// matters.
async fn reindex(path: &std::path::Path) -> String {
    match runtime::runtime().await {
        Ok(rt) => match rt.index_file(path).await {
            Ok(_) => " — searchable now.".to_string(),
            Err(e) => format!(" — written, but not indexed yet ({e})."),
        },
        Err(e) => format!(" — written, but not indexed yet ({e})."),
    }
}

fn relative(path: &std::path::Path) -> String {
    path.strip_prefix(brain::dir())
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

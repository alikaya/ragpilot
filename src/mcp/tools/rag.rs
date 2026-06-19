use serde_json::json;

use crate::indexer::file_language;
use crate::{config::Config, indexer::IndexState};
use crate::store::{SearchFilters, ScoredChunk};
use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tool_definitions(ctx: &McpContext) -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "rag_search",
            "description": ctx.config.mcp.search_tool_description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "k":     { "type": "integer", "description": "Results count (default 6)", "default": 6 },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "path":     { "type": "string", "description": "Glob pattern e.g. src/**/*.rs" },
                            "filetype": { "type": "string", "description": "Extension e.g. rs, py" },
                            "language": { "type": "string", "description": "Language e.g. rust, python" }
                        }
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "rag_get_chunks",
            "description": "Fetch full content of specific chunks by IDs returned from rag_search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "chunk_ids": { "type": "array", "items": { "type": "string" } },
                    "max_chars": { "type": "integer", "default": 2000 }
                },
                "required": ["chunk_ids"]
            }
        }),
        json!({
            "name": "rag_get_file_ranges",
            "description": "Read specific line ranges or symbol definitions from a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path":   { "type": "string" },
                    "ranges": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_line": { "type": "integer" },
                                "end_line":   { "type": "integer" },
                                "symbol":     { "type": "string" }
                            }
                        }
                    }
                },
                "required": ["path", "ranges"]
            }
        }),
        json!({
            "name": "rag_get_skeleton",
            "description": "Return a token-efficient skeleton of a file: signatures, struct/enum/type definitions, imports and doc comments, with function bodies elided to '...'. Prefer this over reading whole files when you only need to understand a file's structure.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        }),
    ]
}

// ─── rag_get_skeleton ─────────────────────────────────────────────────────────

pub fn get_skeleton(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let rel_path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p.trim_start_matches('/'),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'path'".into()),
    };

    let full_path = ctx.root.join(rel_path);
    if !full_path.starts_with(&ctx.root) {
        return McpResponse::tool_error(req.id.clone(), "Path outside project root".into());
    }

    let content = match std::fs::read_to_string(&full_path) {
        Ok(c)  => c,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Cannot read '{rel_path}': {e}")),
    };

    let ext = std::path::Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let language = file_language(ext);
    let skeleton = crate::skeleton::skeletonize(&content, language);

    let full_tokens = crate::tokens::estimate(&content);
    let skeleton_tokens = crate::tokens::estimate(&skeleton);
    let reduction_ratio = if skeleton_tokens == 0 {
        0.0
    } else {
        (full_tokens as f64 / skeleton_tokens as f64 * 100.0).round() / 100.0
    };

    let out = json!({
        "path":            rel_path,
        "language":        language,
        "full_tokens":     full_tokens,
        "skeleton_tokens": skeleton_tokens,
        "reduction_ratio": reduction_ratio,
        "skeleton":        skeleton,
    });
    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&out).unwrap_or_default())
}

// ─── rag_search ──────────────────────────────────────────────────────────────

pub async fn search(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'query'".into()),
    };

    let max_limit = ctx.config.mcp.max_context_chunks.max(1) as u64;
    let limit = args.get("k")
        .and_then(|v| v.as_u64())
        .unwrap_or(ctx.config.mcp.context_chunks as u64)
        .min(max_limit);

    let fv        = args.get("filters");
    let path_glob = fv.and_then(|f| f.get("path")).and_then(|v| v.as_str()).map(|s| s.to_string());
    let language  = fv
        .and_then(|f| f.get("language").or_else(|| f.get("filetype")))
        .and_then(|v| v.as_str())
        .map(|s| if s.len() <= 5 && !s.contains(' ') { file_language(s).to_string() } else { s.to_string() });

    let embeddings = match ctx.embedder.embed(&[query.clone()]).await {
        Ok(e)  => e,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Embedding error: {e}")),
    };
    let query_vec = match embeddings.into_iter().next() {
        Some(v) => v,
        None    => return McpResponse::tool_error(req.id.clone(), "No embedding produced".into()),
    };

    let results = match ctx.store.search(&query_vec, SearchFilters { path_glob, language, limit }).await {
        Ok(r)  => r,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Search error: {e}")),
    };

    if results.is_empty() {
        return McpResponse::tool_text(req.id.clone(),
            "No results found. Run 'ragpilot init' to index the project.".into());
    }

    let items: Vec<serde_json::Value> = results.iter().map(format_result).collect();
    let mut out = serde_json::to_string_pretty(&items).unwrap_or_default();
    if has_dirty_files(ctx) {
        out.push_str("\n\nIndex may be stale. Run rag_ensure_index or ragpilot update.");
    }
    McpResponse::tool_text(req.id.clone(), out)
}

pub fn format_result(r: &ScoredChunk) -> serde_json::Value {
    let snippet = clamp_str(&r.chunk.content, 400);
    let mut obj = json!({
        "chunk_id":   r.chunk.id,
        "path":       r.chunk.source,
        "score":      (r.score * 1000.0).round() / 1000.0,
        "start_line": r.chunk.start_line,
        "end_line":   r.chunk.end_line,
        "language":   r.chunk.language,
        "snippet":    snippet,
    });
    if let Some(ref sym) = r.chunk.symbol {
        obj["symbol"] = json!(sym);
    }
    obj
}

pub fn clamp_str(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{}…", taken) } else { taken }
}

// ─── rag_get_chunks ──────────────────────────────────────────────────────────

pub async fn get_chunks(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let ids: Vec<&str> = match args.get("chunk_ids").and_then(|v| v.as_array()) {
        Some(a) => a.iter().filter_map(|v| v.as_str()).collect(),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'chunk_ids'".into()),
    };
    let max_chars = args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

    let chunks = match ctx.store.get_chunks_by_ids(&ids).await {
        Ok(c)  => c,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Fetch error: {e}")),
    };

    if chunks.is_empty() {
        return McpResponse::tool_text(req.id.clone(), "No chunks found for given IDs.".into());
    }

    let items: Vec<serde_json::Value> = chunks.iter().map(|c| json!({
        "chunk_id":   c.id,
        "path":       c.source,
        "start_line": c.start_line,
        "end_line":   c.end_line,
        "language":   c.language,
        "content":    clamp_str(&c.content, max_chars),
    })).collect();

    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&items).unwrap_or_default())
}

// ─── rag_get_file_ranges ──────────────────────────────────────────────────────

pub fn get_file_ranges(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let rel_path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p.trim_start_matches('/'),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'path'".into()),
    };
    let ranges = match args.get("ranges").and_then(|v| v.as_array()) {
        Some(r) => r,
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'ranges'".into()),
    };

    let full_path = ctx.root.join(rel_path);
    if !full_path.starts_with(&ctx.root) {
        return McpResponse::tool_error(req.id.clone(), "Path outside project root".into());
    }

    let content = match std::fs::read_to_string(&full_path) {
        Ok(c)  => c,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Cannot read '{rel_path}': {e}")),
    };
    let lines: Vec<&str> = content.lines().collect();

    let mut parts: Vec<serde_json::Value> = Vec::new();

    for range in ranges {
        if let (Some(start), Some(end)) = (
            range.get("start_line").and_then(|v| v.as_u64()),
            range.get("end_line").and_then(|v| v.as_u64()),
        ) {
            let s = ((start as usize).saturating_sub(1)).min(lines.len());
            let e = (end as usize).min(lines.len());
            parts.push(json!({
                "path": rel_path, "start_line": start, "end_line": end,
                "content": lines[s..e].join("\n"),
            }));
        } else if let Some(sym) = range.get("symbol").and_then(|v| v.as_str()) {
            if let Some((s, e)) = find_symbol_range(&lines, sym) {
                parts.push(json!({
                    "path": rel_path, "symbol": sym,
                    "start_line": s + 1, "end_line": e,
                    "content": lines[s..e].join("\n"),
                }));
            } else {
                parts.push(json!({ "path": rel_path, "symbol": sym, "error": "not found" }));
            }
        }
    }

    if parts.is_empty() {
        return McpResponse::tool_text(req.id.clone(), "No ranges extracted.".into());
    }
    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&parts).unwrap_or_default())
}

fn find_symbol_range(lines: &[&str], symbol: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| {
        let t = line.trim();
        t.contains(symbol) && (
            t.starts_with("fn ")     || t.starts_with("pub fn ")   ||
            t.starts_with("async fn")|| t.starts_with("def ")      ||
            t.starts_with("class ")  || t.starts_with("function ") ||
            t.starts_with("func ")   || t.starts_with("struct ")   ||
            t.starts_with("impl ")   || t.starts_with("trait ")    ||
            t.starts_with("enum ")   || t.starts_with("export ")
        )
    })?;
    let end = (start + 60).min(lines.len());
    Some((start, end))
}

fn has_dirty_files(ctx: &McpContext) -> bool {
    let state_path = Config::state_path(&ctx.root);
    let state = match IndexState::load(&state_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    state.file_hashes.iter().any(|(rel, stored_hash)| {
        match std::fs::read_to_string(ctx.root.join(rel.as_str())) {
            Ok(c) => crate::indexer::compute_hash(&c) != *stored_hash,
            Err(_) => true,
        }
    })
}

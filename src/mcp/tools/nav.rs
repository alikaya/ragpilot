use serde_json::json;

use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "nav_symbol_resolve",
            "description": "Find where a symbol (function, class, struct, etc.) is defined. Returns file path, line number, and call graph edges.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to look up" }
                },
                "required": ["symbol"]
            }
        }),
        json!({
            "name": "nav_call_graph",
            "description": "Return the call graph around a symbol: what it calls and what calls it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "depth":  { "type": "integer", "default": 2 }
                },
                "required": ["symbol"]
            }
        }),
    ]
}

// ─── nav_symbol_resolve ──────────────────────────────────────────────────────

pub async fn symbol_resolve(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let symbol = match args.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'symbol'".into()),
    };

    let matches = match ctx.symbol_graph.resolve(&symbol).await {
        Ok(m)  => m,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Symbol resolve error: {e}")),
    };

    if matches.is_empty() {
        return McpResponse::tool_text(
            req.id.clone(),
            format!("Symbol '{}' not found. Run 'ragpilot init' to build the symbol index.", symbol),
        );
    }

    // For each match, also get call edges
    let mut results = Vec::new();
    for sym in &matches {
        let callees = ctx.symbol_graph.callees(&sym.id).await.unwrap_or_default();
        let callers = ctx.symbol_graph.callers(&sym.name).await.unwrap_or_default();

        results.push(json!({
            "symbol":     sym.name,
            "kind":       sym.kind,
            "path":       sym.path,
            "start_line": sym.start_line,
            "end_line":   sym.end_line,
            "calls":      callees.iter().map(|c| json!({
                "symbol": c.callee_name, "line": c.call_line
            })).collect::<Vec<_>>(),
            "called_by":  callers.iter().map(|c| json!({
                "symbol": c.caller_id.split("::").last().unwrap_or(&c.caller_id),
                "path":   c.caller_id.split("::").next().unwrap_or(""),
                "line":   c.call_line
            })).collect::<Vec<_>>(),
        }));
    }

    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&results).unwrap_or_default())
}

// ─── nav_call_graph ──────────────────────────────────────────────────────────

pub async fn call_graph(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let symbol = match args.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'symbol'".into()),
    };
    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(ctx.config.symbol_graph.max_depth as u64) as usize;
    let depth = depth.min(ctx.config.symbol_graph.max_depth);
    let max_nodes = ctx.config.symbol_graph.max_nodes.max(1);

    // Resolve the symbol to get its ID
    let resolved = match ctx.symbol_graph.resolve(&symbol).await {
        Ok(v)  => v,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Resolve error: {e}")),
    };

    if resolved.is_empty() {
        return McpResponse::tool_text(req.id.clone(),
            format!("Symbol '{}' not found in index.", symbol));
    }

    let root_sym = &resolved[0];

    // BFS outgoing calls up to `depth`
    let mut calls_bfs: Vec<serde_json::Value> = Vec::new();
    let mut visited_ids = std::collections::HashSet::new();
    let mut frontier = vec![root_sym.id.clone()];

    let mut truncated = false;
    for _ in 0..depth {
        let mut next_frontier = Vec::new();
        for sid in &frontier {
            let edges = ctx.symbol_graph.callees(sid).await.unwrap_or_default();
            for edge in edges {
                if calls_bfs.len() >= max_nodes {
                    truncated = true;
                    break;
                }
                if !visited_ids.contains(&edge.callee_name) {
                    visited_ids.insert(edge.callee_name.clone());
                    // Look up callee location
                    let loc = ctx.symbol_graph.resolve(&edge.callee_name).await.unwrap_or_default();
                    let path = loc.first().map(|s| s.path.as_str()).unwrap_or("");
                    let line = loc.first().map(|s| s.start_line).unwrap_or(0);
                    calls_bfs.push(json!({
                        "symbol": edge.callee_name, "path": path,
                        "line": line, "call_line": edge.call_line,
                    }));
                    if let Some(s) = loc.first() {
                        next_frontier.push(s.id.clone());
                    }
                }
            }
            if truncated {
                break;
            }
        }
        frontier = next_frontier;
        if truncated {
            break;
        }
    }

    // Incoming callers (1 hop)
    let callers = ctx.symbol_graph.callers(&root_sym.name).await.unwrap_or_default();
    let called_by: Vec<serde_json::Value> = callers.iter().map(|c| {
        let path = c.caller_id.split("::").next().unwrap_or("");
        json!({
            "symbol": c.caller_id.split("::").last().unwrap_or(&c.caller_id),
            "path":   path,
            "line":   c.call_line,
        })
    }).collect();

    let result = json!({
        "symbol":    root_sym.name,
        "path":      root_sym.path,
        "kind":      root_sym.kind,
        "calls":     calls_bfs,
        "called_by": called_by,
        "truncated": truncated,
    });

    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&result).unwrap_or_default())
}

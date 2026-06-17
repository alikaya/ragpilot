use serde_json::json;

use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "impact.analyze",
        "description": "Given symbols or file paths, return which files and symbols would be affected by changes. Use before refactoring.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbols": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Symbol names to analyze"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "File paths to analyze"
                }
            }
        }
    })]
}

pub async fn analyze(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let symbol_names: Vec<String> = args.get("symbols")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let paths: Vec<String> = args.get("paths")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    if symbol_names.is_empty() && paths.is_empty() {
        return McpResponse::tool_error(req.id.clone(), "Provide at least one 'symbol' or 'path'".into());
    }

    // Collect all affected paths via ImpactIndex
    let mut all_changed: Vec<String> = paths.clone();

    // For each symbol, find its path and add to changed set
    for sym_name in &symbol_names {
        if let Ok(matches) = ctx.symbol_graph.resolve(sym_name).await {
            for sym in matches {
                if !all_changed.contains(&sym.path) {
                    all_changed.push(sym.path);
                }
            }
        }
    }

    let max_hops = ctx.config.symbol_graph.max_depth.min(2);
    let affected_files = match ctx.impact_index.get_affected_transitive(&all_changed, max_hops).await {
        Ok(f)  => f,
        Err(e) => return McpResponse::tool_error(req.id.clone(), format!("Impact analysis error: {e}")),
    };
    let max_nodes = ctx.config.symbol_graph.max_nodes.max(1);
    let truncated = affected_files.len() > max_nodes;
    let affected_files: Vec<String> = affected_files.into_iter().take(max_nodes).collect();

    // Collect affected symbols from affected files
    let mut affected_symbols: Vec<serde_json::Value> = Vec::new();
    for file_path in &affected_files {
        if let Ok(syms) = ctx.symbol_graph.symbols_in_file(file_path).await {
            for s in syms {
                affected_symbols.push(json!({
                    "symbol": s.name, "kind": s.kind, "path": s.path,
                    "start_line": s.start_line,
                }));
            }
        }
    }

    // Breaking signals: if changed symbols are exported (pub/export)
    let mut breaking_signals: Vec<String> = Vec::new();
    for sym_name in &symbol_names {
        if let Ok(matches) = ctx.symbol_graph.resolve(sym_name).await {
            for sym in matches {
                if sym.kind == "function" || sym.kind == "struct" || sym.kind == "trait" {
                    if !affected_files.is_empty() {
                        breaking_signals.push(format!(
                            "Changing '{}' in {} may break {} dependent file(s)",
                            sym.name, sym.path, affected_files.len()
                        ));
                    }
                }
            }
        }
    }

    let mut result = affected_files.clone();
    result.sort();

    let output = json!({
        "changed_paths":     all_changed,
        "affected_files":    result,
        "affected_symbols":  affected_symbols,
        "breaking_signals":  breaking_signals,
        "truncated":         truncated,
    });

    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&output).unwrap_or_default())
}

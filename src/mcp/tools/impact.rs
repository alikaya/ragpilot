use std::collections::HashSet;

use serde_json::json;

use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};
use crate::parser::Symbol;

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "impact_analyze",
        "description": "Given symbols or file paths, return which files and symbols would be affected by changes. Walks the call graph transitively (who calls the changed code, and who calls those callers) and the import graph. Use before refactoring.",
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

/// A symbol reached by walking incoming call edges from the changed set.
struct AffectedSym {
    name:     String,
    path:     String,
    line:     usize,
    distance: usize,
    via:      String,
}

/// Whether `name` has more than one definition in the project. Counts are
/// resolved lazily and memoised in `def_counts` so repeated frontier checks
/// stay cheap.
async fn is_ambiguous(
    ctx: &McpContext,
    def_counts: &mut std::collections::HashMap<String, usize>,
    name: &str,
) -> bool {
    if let Some(&n) = def_counts.get(name) {
        return n > 1;
    }
    let n = ctx.symbol_graph.resolve(name).await.map(|v| v.len()).unwrap_or(0);
    def_counts.insert(name.to_string(), n);
    n > 1
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

    // ── Seed set: the symbols considered "changed" ─────────────────────────
    // Explicit symbols are resolved; for paths, every symbol defined in the
    // file is a seed (changing the file may change any of them).
    let mut seeds: Vec<Symbol> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for sym_name in &symbol_names {
        match ctx.symbol_graph.resolve(sym_name).await {
            Ok(matches) if !matches.is_empty() => seeds.extend(matches),
            _ => unresolved.push(sym_name.clone()),
        }
    }
    for path in &paths {
        if let Ok(syms) = ctx.symbol_graph.symbols_in_file(path).await {
            seeds.extend(syms);
        }
    }
    // Dedupe seeds by id.
    {
        let mut seen = HashSet::new();
        seeds.retain(|s| seen.insert(s.id.clone()));
    }

    let mut all_changed: Vec<String> = paths.clone();
    for s in &seeds {
        if !all_changed.contains(&s.path) {
            all_changed.push(s.path.clone());
        }
    }

    // ── Transitive caller walk (the same edges nav_call_graph uses) ────────
    // Call edges match by name; a name defined in several places (e.g. `new`,
    // `remove`) would attribute every same-named call in the project to this
    // seed. Such ambiguous names are not walked — except when the user asked
    // for them explicitly — and a signal reports the ambiguity instead.
    let max_hops  = ctx.config.symbol_graph.max_depth.clamp(1, 5);
    let max_nodes = ctx.config.symbol_graph.max_nodes.max(1);

    let explicit: HashSet<String> = symbol_names.iter().cloned().collect();
    // name → project-wide definition count, resolved lazily (see is_ambiguous).
    let mut def_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    let mut ambiguous_skipped: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = seeds.iter().map(|s| s.id.clone()).collect();
    let mut affected: Vec<AffectedSym> = Vec::new();
    let mut frontier: Vec<String> = {
        let mut names: Vec<String> = Vec::new();
        for s in &seeds {
            if names.contains(&s.name) {
                continue;
            }
            let ambiguous = is_ambiguous(ctx, &mut def_counts, &s.name).await;
            if ambiguous && !explicit.contains(&s.name) {
                if !ambiguous_skipped.contains(&s.name) {
                    ambiguous_skipped.push(s.name.clone());
                }
                continue;
            }
            names.push(s.name.clone());
        }
        names.sort();
        names
    };
    let mut truncated = false;

    'walk: for hop in 1..=max_hops {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        for callee in &frontier {
            let callers = match ctx.symbol_graph.callers(callee).await {
                Ok(c)  => c,
                Err(_) => continue,
            };
            for call in callers {
                // caller_id format: "<path>::<symbol name>"; paths never
                // contain "::", so the first separator splits correctly.
                let Some((path, name)) = call.caller_id.split_once("::") else { continue };
                if !visited.insert(call.caller_id.clone()) {
                    continue;
                }
                affected.push(AffectedSym {
                    name:     name.to_string(),
                    path:     path.to_string(),
                    line:     call.call_line,
                    distance: hop,
                    via:      callee.clone(),
                });
                if !is_ambiguous(ctx, &mut def_counts, name).await {
                    next.push(name.to_string());
                }
                if affected.len() >= max_nodes {
                    truncated = true;
                    break 'walk;
                }
            }
        }
        next.sort();
        next.dedup();
        frontier = next;
    }

    // ── File-level impact: caller files + import-graph dependents ──────────
    let mut affected_files: HashSet<String> = affected
        .iter()
        .map(|a| a.path.clone())
        .filter(|p| !all_changed.contains(p))
        .collect();
    if let Ok(import_affected) = ctx.impact_index.get_affected_transitive(&all_changed, max_hops).await {
        for f in import_affected {
            if !all_changed.contains(&f) {
                affected_files.insert(f);
            }
        }
    }
    let mut affected_files: Vec<String> = affected_files.into_iter().collect();
    affected_files.sort();

    // ── Breaking signals: per changed symbol, its real direct callers ──────
    let mut breaking_signals: Vec<String> = Vec::new();
    for seed in &seeds {
        let direct: Vec<&AffectedSym> = affected
            .iter()
            .filter(|a| a.distance == 1 && a.via == seed.name)
            .collect();
        if direct.is_empty() {
            continue;
        }
        let files: HashSet<&str> = direct.iter().map(|a| a.path.as_str()).collect();
        let mut names: Vec<String> = direct.iter().map(|a| a.name.clone()).collect();
        names.sort();
        names.dedup();
        let sample = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        let more = if names.len() > 3 { format!(", +{} more", names.len() - 3) } else { String::new() };
        breaking_signals.push(format!(
            "Changing '{}' ({}, {}) impacts {} direct caller(s) in {} file(s): {}{}",
            seed.name, seed.kind, seed.path, direct.len(), files.len(), sample, more
        ));
    }
    for name in &unresolved {
        breaking_signals.push(format!(
            "Symbol '{}' was not found in the symbol graph — impact for it is unknown (re-run rag_ensure_index or check the name)",
            name
        ));
    }
    for name in &ambiguous_skipped {
        breaking_signals.push(format!(
            "Symbol '{}' is defined in multiple places — name-based caller matching would be unreliable, so its call-graph walk was skipped (pass it explicitly in 'symbols' to force it)",
            name
        ));
    }
    for name in &symbol_names {
        if is_ambiguous(ctx, &mut def_counts, name).await {
            breaking_signals.push(format!(
                "Note: '{}' has multiple definitions in the project — some listed callers may refer to the other definitions",
                name
            ));
        }
    }

    let affected_symbols: Vec<serde_json::Value> = affected
        .iter()
        .map(|a| json!({
            "symbol":    a.name,
            "path":      a.path,
            "call_line": a.line,
            "distance":  a.distance,
            "via":       a.via,
        }))
        .collect();

    let output = json!({
        "changed_paths":     all_changed,
        "affected_files":    affected_files,
        "affected_symbols":  affected_symbols,
        "breaking_signals":  breaking_signals,
        "truncated":         truncated,
    });

    McpResponse::tool_text(req.id.clone(), serde_json::to_string_pretty(&output).unwrap_or_default())
}

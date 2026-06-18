use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Instant;

use super::McpContext;
use crate::config::Config;
use crate::indexer::{file_language, BundleTokenStats, IndexState};
use crate::mcp::protocol::{McpRequest, McpResponse};
use crate::store::SearchFilters;
use super::rag::format_result;
use tiktoken_rs::CoreBPE;

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "context.bundle",
        "description": "Single call to get a complete, token-budgeted context package for a task. Combines semantic search results, relevant symbols, impact summary, and a project tree snapshot. Call this FIRST before any other tool.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task":          { "type": "string", "description": "Task description for context retrieval" },
                "budget_tokens": { "type": "integer", "description": "Max output tokens (default: from config)", "default": 6000 }
            },
            "required": ["task"]
        }
    })]
}

pub async fn bundle(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let started = Instant::now();
    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None    => return McpResponse::tool_error(req.id.clone(), "Missing 'task'".into()),
    };

    let budget = args.get("budget_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(ctx.config.mcp.bundle_budget_tokens as u64) as usize;
    let budget = budget.min(ctx.config.mcp.max_context_tokens.max(1));

    let mut used_tokens = 0usize;
    let mut output = serde_json::Map::new();

    // ── 1. Semantic search ────────────────────────────────────────────────────
    let embeddings = ctx.embedder.embed(&[task.clone()]).await.unwrap_or_default();
    let rag_chunks = if let Some(vec) = embeddings.into_iter().next() {
        let limit = ctx
            .config
            .mcp
            .context_chunks
            .min(ctx.config.mcp.max_context_chunks)
            .max(1) as u64;
        ctx.store.search(&vec, SearchFilters { limit, ..Default::default() }).await.unwrap_or_default()
    } else {
        vec![]
    };
    let max_files = ctx.config.mcp.max_context_files.max(1);
    let max_chunks = ctx.config.mcp.max_context_chunks.max(1);

    let chunk_limit = budget * 60 / 100;
    let mut chunk_items: Vec<serde_json::Value> = Vec::new();
    let mut selected_chunks_estimated_tokens = 0usize;
    let per_file_cap = 3usize;
    let mut per_file_used: HashMap<&str, usize> = HashMap::new();
    let candidate_chunks_estimated_tokens: usize = rag_chunks
        .iter()
        .map(|r| estimate_tokens_for_text(&r.chunk.content, &r.chunk.language))
        .sum();
    for r in rag_chunks.iter().take(max_chunks) {
        if used_tokens >= chunk_limit { break; }
        let used = per_file_used.entry(r.chunk.source.as_str()).or_insert(0);
        if *used >= per_file_cap {
            continue;
        }
        let v = format_result(r);
        used_tokens += approx_tokens(&v.to_string());
        selected_chunks_estimated_tokens += estimate_tokens_for_text(&r.chunk.content, &r.chunk.language);
        chunk_items.push(v);
        *used += 1;
    }

    output.insert("rag_chunks".into(), json!(chunk_items));

    // ── 2. Symbols from matched files ─────────────────────────────────────────
    let matched_paths: HashSet<String> = rag_chunks.iter()
        .map(|r| r.chunk.source.clone())
        .take(max_files)
        .collect();

    let mut symbols_out = Vec::new();
    for path in &matched_paths {
        if used_tokens >= budget * 80 / 100 { break; }
        if let Ok(syms) = ctx.symbol_graph.symbols_in_file(path).await {
            for s in syms {
                let v = json!({ "symbol": s.name, "kind": s.kind, "path": s.path, "line": s.start_line });
                used_tokens += approx_tokens(&v.to_string());
                symbols_out.push(v);
            }
        }
    }
    output.insert("symbols".into(), json!(symbols_out));

    // ── 3. Impact summary ─────────────────────────────────────────────────────
    if used_tokens < budget * 90 / 100 {
        let paths: Vec<String> = matched_paths.iter().cloned().collect();
        if let Ok(affected) = ctx.impact_index.get_affected_transitive(&paths, 1).await {
            let summary = if affected.is_empty() {
                "No downstream dependents detected.".to_string()
            } else {
                format!("{} file(s) depend on the matched code: {}", affected.len(),
                    affected.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
            };
            used_tokens += approx_tokens(&summary);
            output.insert("impact_summary".into(), json!(summary));
        }
    }

    // ── 4. Tree snapshot (parent dirs of matched files) ───────────────────────
    if used_tokens < budget {
        let dirs: HashSet<String> = matched_paths.iter()
            .filter_map(|p| std::path::Path::new(p).parent().map(|d| d.to_string_lossy().to_string()))
            .collect();

        let mut tree_out = Vec::new();
        for dir in dirs.iter().take(3) {
            if used_tokens >= budget { break; }
            if let Ok(paths) = ctx.project_tree.paths_in_dir(dir, 2).await {
                for p in paths.iter().take(20) {
                    tree_out.push(p.clone());
                    used_tokens += p.len() / 4 + 1;
                }
            }
        }
        output.insert("tree_snapshot".into(), json!(tree_out));
    }

    // ── 5. Token-saving metrics ───────────────────────────────────────────────
    // Honest baseline: what it would cost to read the matched files WHOLE — the
    // "no-RAG" counterfactual. Measured with the SAME tokenizer as the delivered
    // bundle so the ratio is comparable. Note: this is an UPPER BOUND on saving
    // vs. a disciplined partial read, and ignores any follow-up tool calls.
    let full_file_baseline_tokens: usize = matched_paths
        .iter()
        .filter_map(|rel| {
            std::fs::read_to_string(ctx.root.join(rel))
                .ok()
                .map(|content| estimate_tokens_for_text(&content, &file_language(rel)))
        })
        .sum();
    let saving_vs_full_file_tokens = full_file_baseline_tokens.saturating_sub(used_tokens);
    let saving_vs_full_file_percent = if full_file_baseline_tokens == 0 {
        0.0
    } else {
        (saving_vs_full_file_tokens as f64 * 100.0) / full_file_baseline_tokens as f64
    };
    let saving_ratio = if used_tokens == 0 {
        0.0
    } else {
        full_file_baseline_tokens as f64 / used_tokens as f64
    };

    // Secondary tuning metric: how much the budget cap trimmed the retrieved
    // chunk set. NOT a value measure — a higher number just means more was
    // dropped (a smaller budget inflates it), so never read it as "savings".
    let budget_trimmed_tokens =
        candidate_chunks_estimated_tokens.saturating_sub(selected_chunks_estimated_tokens);
    let budget_trim_percent = if candidate_chunks_estimated_tokens == 0 {
        0.0
    } else {
        (budget_trimmed_tokens as f64 * 100.0) / candidate_chunks_estimated_tokens as f64
    };

    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    output.insert("approx_tokens_used".into(), json!(used_tokens));
    output.insert("estimated_tokens".into(), json!(used_tokens));
    output.insert(
        "full_file_baseline_tokens".into(),
        json!(full_file_baseline_tokens),
    );
    output.insert(
        "saving_vs_full_file_tokens".into(),
        json!(saving_vs_full_file_tokens),
    );
    output.insert(
        "saving_vs_full_file_percent".into(),
        json!(round2(saving_vs_full_file_percent)),
    );
    output.insert("saving_ratio".into(), json!(round2(saving_ratio)));
    output.insert(
        "candidate_chunks_estimated_tokens".into(),
        json!(candidate_chunks_estimated_tokens),
    );
    output.insert(
        "selected_chunks_estimated_tokens".into(),
        json!(selected_chunks_estimated_tokens),
    );
    output.insert("budget_trim_percent".into(), json!(round2(budget_trim_percent)));

    let state_path = Config::state_path(&ctx.root);
    let mut state = IndexState::load(&state_path).unwrap_or_default();
    state.last_bundle_token_stats = Some(BundleTokenStats {
        task: task.clone(),
        generated_at: Some(chrono::Utc::now()),
        duration_ms: started.elapsed().as_millis(),
        estimated_tokens: used_tokens,
        full_file_baseline_tokens,
        saving_vs_full_file_tokens,
        saving_vs_full_file_percent: round2(saving_vs_full_file_percent),
        saving_ratio: round2(saving_ratio),
        candidate_chunks_estimated_tokens,
        selected_chunks_estimated_tokens,
        budget_trim_percent: round2(budget_trim_percent),
    });
    let _ = state.save(&state_path);

    McpResponse::tool_text(
        req.id.clone(),
        serde_json::to_string_pretty(&serde_json::Value::Object(output)).unwrap_or_default(),
    )
}

fn approx_tokens(s: &str) -> usize {
    estimate_tokens_for_text(s, "text")
}

fn estimate_tokens_for_text(text: &str, language: &str) -> usize {
    if let Some(token_count) = estimate_tokens_with_tokenizer(text) {
        return token_count;
    }

    let chars = text.chars().count() as f64;
    let ratio = if is_probably_turkish(text) {
        3.0
    } else if is_code_language(language) {
        3.5
    } else {
        4.0
    };
    (chars / ratio).ceil() as usize
}

fn is_code_language(language: &str) -> bool {
    !matches!(language, "markdown" | "text")
}

fn is_probably_turkish(text: &str) -> bool {
    let lowered = text.to_lowercase();
    let tr_markers = [" ve ", " için ", " bir ", " ile ", "ş", "ğ", "ı", "ç", "ö", "ü"];
    tr_markers.iter().any(|m| lowered.contains(m))
}

fn estimate_tokens_with_tokenizer(text: &str) -> Option<usize> {
    static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
    let bpe = CL100K
        .get_or_init(|| tiktoken_rs::cl100k_base().ok())
        .as_ref()?;
    Some(bpe.encode_with_special_tokens(text).len())
}

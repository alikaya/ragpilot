use serde_json::json;

use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "review_semantic_diff",
        "description": "Semantic diff of code changes: which SYMBOLS changed \
            (added / removed / signature_changed / modified) and their blast \
            radius — callers (from the symbol graph) and dependent files (from \
            the import graph). Use this for PR/commit review and to write \
            accurate commit messages (e.g. \"changed the return type of X, which \
            affects Y and Z\"). Defaults to the working tree vs HEAD; pass a ref \
            like \"HEAD~1\" or a range like \"main..HEAD\".",
        "inputSchema": {
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Git ref or range (default: working tree vs HEAD)"
                }
            }
        }
    })]
}

pub async fn semantic_diff(req: &McpRequest, args: &serde_json::Value, ctx: &McpContext) -> McpResponse {
    let target = args.get("ref").and_then(|v| v.as_str());
    match crate::semantic_diff::analyze(&ctx.root, target).await {
        Ok(report) => McpResponse::tool_text(
            req.id.clone(),
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        ),
        Err(e) => McpResponse::tool_error(req.id.clone(), format!("semantic diff failed: {e}")),
    }
}

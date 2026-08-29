use serde_json::json;

use super::McpContext;
use crate::mcp::protocol::{McpRequest, McpResponse};

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![json!({
        "name": "review_semantic_diff",
        "description": "Which symbols changed (added/removed/signature/modified) and their blast radius — callers and dependent files. For reviewing a diff or writing a commit message.",
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

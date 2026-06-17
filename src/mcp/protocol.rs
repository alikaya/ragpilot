use serde::{Deserialize, Serialize};

/// Inbound JSON-RPC 2.0 message from the MCP client (Claude Code).
#[derive(Debug, Deserialize)]
pub struct McpRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// None when the message is a notification (no response expected).
    pub id:      Option<serde_json::Value>,
    pub method:  String,
    pub params:  Option<serde_json::Value>,
}

impl McpRequest {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// Outbound JSON-RPC 2.0 message to the MCP client.
#[derive(Debug, Serialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id:      Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result:  Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error:   Option<McpError>,
}

#[derive(Debug, Serialize)]
pub struct McpError {
    pub code:    i32,
    pub message: String,
}

impl McpResponse {
    pub fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(code: i32, message: &str, id: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(McpError { code, message: message.into() }),
        }
    }

    /// Wraps a tool result string as MCP content array.
    pub fn tool_text(id: Option<serde_json::Value>, text: String) -> Self {
        Self::ok(id, serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }))
    }

    /// Wraps a tool error as MCP isError content.
    pub fn tool_error(id: Option<serde_json::Value>, message: String) -> Self {
        Self::ok(id, serde_json::json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }))
    }
}

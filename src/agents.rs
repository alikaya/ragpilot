//! MCP client registration.
//!
//! Each supported coding agent / IDE discovers MCP servers from its own config
//! file in its own format. This module writes the `ragpilot` registration into
//! the right place for each, migrating any legacy `rag` key written by older
//! versions. The server key and command are ALWAYS `ragpilot` — never `rag`.
//!
//! Project-level clients (config lives in the repo) get their file written.
//! Global-only clients (Windsurf, Antigravity — config lives in $HOME and would
//! affect every project) are NOT written; instead we print the exact snippet to
//! paste, so `init` never silently touches files outside the repo.

use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use serde_json::{json, Map, Value};

/// Clients that write a per-project config file.
pub const PROJECT_CLIENTS: &[&str] = &["claude", "codex", "cursor", "vscode", "opencode"];
/// Clients that only support a global ($HOME) config — handled via snippet.
pub const GLOBAL_CLIENTS: &[&str] = &["windsurf", "antigravity"];

/// Write (or migrate) the ragpilot MCP registration for `agent`.
pub fn configure(agent: &str, root: &Path) -> Result<()> {
    match agent.to_lowercase().as_str() {
        "claude" => claude(root),
        "codex" => codex(root),
        "cursor" => cursor(root),
        "vscode" | "vs-code" | "code" => vscode(root),
        "opencode" => opencode(root),

        // Gemini CLI was deprecated on 2026-06-18 in favour of the Antigravity
        // CLI (binary `agy`). Redirect with a clear notice.
        "gemini" | "gemini-cli" => {
            println!(
                "{} Gemini CLI was deprecated on 2026-06-18 → redirecting to the Antigravity CLI.",
                "⚠".yellow()
            );
            antigravity(root)
        }
        "antigravity" | "antigravity-cli" | "agy" => antigravity(root),

        "windsurf" => {
            global_snippet(
                "Windsurf",
                "~/.codeium/windsurf/mcp_config.json",
                "mcpServers",
                false,
                None,
                root,
            );
            Ok(())
        }

        "all" => {
            for a in PROJECT_CLIENTS {
                configure(a, root)?;
            }
            for a in GLOBAL_CLIENTS {
                configure(a, root)?;
            }
            Ok(())
        }

        other => anyhow::bail!(
            "Unknown agent '{}'.\n  Supported: claude, codex, cursor, vscode, opencode, windsurf, antigravity, all",
            other
        ),
    }
}

// ─── Per-client writers ────────────────────────────────────────────────────────

fn claude(root: &Path) -> Result<()> {
    write_json_mcp(&root.join(".mcp.json"), "mcpServers", server_entry(true), ".mcp.json", &[])?;
    write_doc(&root.join("CLAUDE.md"), crate::CLAUDE_MD, "CLAUDE.md")
}

fn opencode(root: &Path) -> Result<()> {
    // opencode: project `opencode.json`, root key `mcp`, and a distinct entry
    // shape — `command` is an ARRAY (binary + args) with `type: "local"`.
    let entry = json!({
        "type":    "local",
        "command": ["ragpilot", "--mcp-server"],
        "enabled": true
    });
    let schema = ("$schema", json!("https://opencode.ai/config.json"));
    write_json_mcp(&root.join("opencode.json"), "mcp", entry, "opencode.json", &[schema])?;
    write_doc(&root.join("AGENTS.md"), crate::AGENTS_MD, "AGENTS.md")
}

fn antigravity(root: &Path) -> Result<()> {
    // Antigravity CLI (binary `agy`) + IDE 2.0 share one GLOBAL config; there is
    // no per-project file. Show the paste-in snippet for the unified path, then
    // write the project context doc so the CLI picks up the RAG-FIRST policy.
    global_snippet(
        "Antigravity CLI/IDE",
        "~/.gemini/config/mcp_config.json",
        "mcpServers",
        false,
        Some("CLI (agy) + IDE 2.0 share this config. CLI-only path: ~/.gemini/antigravity-cli/mcp_config.json"),
        root,
    );
    write_doc(&root.join("AGENTS.md"), crate::AGENTS_MD, "AGENTS.md")
}

fn cursor(root: &Path) -> Result<()> {
    write_json_mcp(&root.join(".cursor/mcp.json"), "mcpServers", server_entry(false), ".cursor/mcp.json", &[])?;
    write_doc(&root.join("AGENTS.md"), crate::AGENTS_MD, "AGENTS.md")
}

fn vscode(root: &Path) -> Result<()> {
    // VS Code is the odd one out: root key is `servers` (NOT `mcpServers`) and
    // an explicit `"type": "stdio"` is expected.
    write_json_mcp(&root.join(".vscode/mcp.json"), "servers", server_entry(true), ".vscode/mcp.json", &[])?;
    write_doc(&root.join("AGENTS.md"), crate::AGENTS_MD, "AGENTS.md")
}

fn codex(root: &Path) -> Result<()> {
    let codex_dir = root.join(".codex");
    let codex_config = codex_dir.join("config.toml");
    std::fs::create_dir_all(&codex_dir)?;

    if codex_config.exists() {
        let raw = std::fs::read_to_string(&codex_config)?;
        if raw.contains("[mcp_servers.rag]") {
            let fixed = raw
                .replace("[mcp_servers.rag]", "[mcp_servers.ragpilot]")
                .replace("command = \"rag\"", "command = \"ragpilot\"");
            std::fs::write(&codex_config, fixed)?;
            println!("{} .codex/config.toml (migrated legacy 'rag' → 'ragpilot')", "✓".green());
        } else if !raw.contains("[mcp_servers.ragpilot]") {
            let mut updated = raw;
            if !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(
                "\n[mcp_servers.ragpilot]\ncommand = \"ragpilot\"\nargs    = [\"--mcp-server\"]\n",
            );
            std::fs::write(&codex_config, updated)?;
            println!("{} .codex/config.toml (ragpilot added)", "✓".green());
        } else {
            println!("{} .codex/config.toml (already exists)", "i".blue());
        }
    } else {
        let root_str = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .to_string();
        let content = format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n\
             [mcp_servers.ragpilot]\ncommand = \"ragpilot\"\nargs    = [\"--mcp-server\"]\n\n\
             # For safety, enabled only in this project\ntrusted = true\n",
            root_str
        );
        std::fs::write(&codex_config, content)?;
        println!("{} .codex/config.toml", "✓".green());
    }

    write_doc(&root.join("AGENTS.md"), crate::AGENTS_MD, "AGENTS.md")
}

// ─── JSON MCP config helpers ───────────────────────────────────────────────────

/// The canonical stdio server entry. `include_type` adds `"type": "stdio"`.
fn server_entry(include_type: bool) -> Value {
    server_entry_with_root(include_type, None)
}

/// Like `server_entry`, but when `root` is given the server is pinned to that
/// project via `--root <abs path>`. Used for global ($HOME) clients that launch
/// the server folder-independently and therefore cannot rely on the cwd.
fn server_entry_with_root(include_type: bool, root: Option<&Path>) -> Value {
    let mut args = vec![json!("--mcp-server")];
    if let Some(r) = root {
        let abs = r.canonicalize().unwrap_or_else(|_| r.to_path_buf());
        args.push(json!("--root"));
        args.push(json!(abs.to_string_lossy()));
    }
    if include_type {
        json!({ "type": "stdio", "command": "ragpilot", "args": args })
    } else {
        json!({ "command": "ragpilot", "args": args })
    }
}

/// Merge-write a JSON MCP config under `root_key` (`mcpServers`, `servers`, or
/// `mcp`). `entry` is the per-server value (its shape varies by client). Creates
/// parent dirs, applies any `top_defaults` (e.g. `$schema`) when missing,
/// migrates a legacy `rag` key, and is idempotent.
fn write_json_mcp(
    path: &Path,
    root_key: &str,
    entry: Value,
    display: &str,
    top_defaults: &[(&str, Value)],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let exists = path.exists();
    let mut doc: Value = if exists {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    // Apply top-level defaults (only when absent) — e.g. opencode's `$schema`.
    for (k, v) in top_defaults {
        if doc.get(*k).is_none() {
            doc[*k] = v.clone();
        }
    }

    let legacy_ptr = format!("/{root_key}/rag");
    let had_legacy = doc.pointer(&legacy_ptr).is_some();
    if had_legacy {
        if let Some(obj) = doc.pointer_mut(&format!("/{root_key}")).and_then(|v| v.as_object_mut()) {
            obj.remove("rag");
        }
    }

    let current_ptr = format!("/{root_key}/ragpilot");
    let up_to_date = doc.pointer(&current_ptr) == Some(&entry);

    if exists && up_to_date && !had_legacy {
        println!("{} {} (ragpilot already registered)", "i".blue(), display);
        return Ok(());
    }

    if !doc.get(root_key).map(Value::is_object).unwrap_or(false) {
        doc[root_key] = json!({});
    }
    doc[root_key]["ragpilot"] = entry;
    std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;

    if !exists {
        println!("{} {}", "✓".green(), display);
    } else if had_legacy {
        println!("{} {} (migrated legacy 'rag' → 'ragpilot')", "✓".green(), display);
    } else {
        println!("{} {} (ragpilot added)", "✓".green(), display);
    }
    Ok(())
}

fn write_doc(path: &Path, content: &str, display: &str) -> Result<()> {
    if path.exists() {
        println!("{} {} (already exists)", "i".blue(), display);
    } else {
        std::fs::write(path, content)?;
        println!("{} {}", "✓".green(), display);
    }
    Ok(())
}

// ─── Global-only clients ───────────────────────────────────────────────────────

/// Print a paste-in snippet for clients that only support a global ($HOME)
/// config — we never write outside the repo during `init`. The snippet pins the
/// server to `root` via `--root`, since a global client launches it folder-
/// independently and cannot rely on the working directory.
fn global_snippet(name: &str, path: &str, root_key: &str, include_type: bool, hint: Option<&str>, root: &Path) {
    let mut servers = Map::new();
    servers.insert("ragpilot".into(), server_entry_with_root(include_type, Some(root)));
    let mut obj = Map::new();
    obj.insert(root_key.into(), Value::Object(servers));
    let snippet = serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default();

    println!(
        "\n{} {} only supports a GLOBAL config (no per-project config).",
        "ℹ".blue(),
        name.bold()
    );
    println!("  Add to this file: {}", path.bold());
    println!("  Pinned to this project via --root {}", root.display().to_string().dimmed());
    if let Some(h) = hint {
        println!("  {}", h.dimmed());
    }
    println!("{}", snippet);
}

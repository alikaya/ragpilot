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

/// The per-project MCP config a client reads, if it has one.
///
/// Kept next to the writers so the two cannot drift: `projects sync` uses this
/// to decide whether a project still needs configuring.
pub fn mcp_config_path(agent: &str, root: &Path) -> Option<std::path::PathBuf> {
    let rel = match agent {
        "claude" => ".mcp.json",
        "cursor" => ".cursor/mcp.json",
        "vscode" => ".vscode/mcp.json",
        "opencode" => "opencode.json",
        "codex" => ".codex/config.toml",
        _ => return None,
    };
    Some(root.join(rel))
}

/// The agent markdown file a client reads.
pub fn agent_doc(agent: &str) -> &'static str {
    if agent == "claude" { "CLAUDE.md" } else { "AGENTS.md" }
}

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
    if crate::brain::exists() {
        crate::brain::hooks::install_claude(root)?;
    }
    upsert_doc(&root.join("CLAUDE.md"), &with_brain_convention(crate::CLAUDE_MD), "CLAUDE.md")
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
    upsert_doc(&root.join("AGENTS.md"), &with_brain_convention(crate::AGENTS_MD), "AGENTS.md")
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
    upsert_doc(&root.join("AGENTS.md"), &with_brain_convention(crate::AGENTS_MD), "AGENTS.md")
}

fn cursor(root: &Path) -> Result<()> {
    write_json_mcp(&root.join(".cursor/mcp.json"), "mcpServers", server_entry(false), ".cursor/mcp.json", &[])?;
    upsert_doc(&root.join("AGENTS.md"), &with_brain_convention(crate::AGENTS_MD), "AGENTS.md")
}

fn vscode(root: &Path) -> Result<()> {
    // VS Code is the odd one out: root key is `servers` (NOT `mcpServers`) and
    // an explicit `"type": "stdio"` is expected.
    write_json_mcp(&root.join(".vscode/mcp.json"), "servers", server_entry(true), ".vscode/mcp.json", &[])?;
    upsert_doc(&root.join("AGENTS.md"), &with_brain_convention(crate::AGENTS_MD), "AGENTS.md")
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

    upsert_doc(&root.join("AGENTS.md"), &with_brain_convention(crate::AGENTS_MD), "AGENTS.md")
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

    // How the server is launched is ours; everything else in the entry is the
    // user's. `env` in particular carries the enterprise reporting credentials
    // on a dev machine, and replacing the entry wholesale silently switched
    // reporting off — which is exactly the kind of failure nobody notices.
    let current_ptr = format!("/{root_key}/ragpilot");
    let existing = doc.pointer(&current_ptr).cloned();
    let merged = merge_entry(existing.as_ref(), &entry);
    let up_to_date = existing.as_ref() == Some(&merged);

    if exists && up_to_date && !had_legacy {
        println!("{} {} (ragpilot already registered)", "i".blue(), display);
        return Ok(());
    }

    if !doc.get(root_key).map(Value::is_object).unwrap_or(false) {
        doc[root_key] = json!({});
    }
    doc[root_key]["ragpilot"] = merged;
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

/// Keys ragpilot owns in a server entry: how the process is started. Anything
/// else the user put there — `env`, `cwd`, a timeout — is theirs to keep.
const OWNED_KEYS: &[&str] = &["type", "command", "args"];

/// Overlay the launch keys onto whatever is already registered.
///
/// Without this, re-registering a project throws away every key ragpilot does
/// not write itself. It did: a fleet-wide `projects sync` deleted the `env`
/// block carrying the enterprise reporting token, and reporting stopped without
/// a single error.
fn merge_entry(existing: Option<&Value>, ours: &Value) -> Value {
    let (Some(Value::Object(existing)), Value::Object(ours)) = (existing, ours) else {
        return ours.clone();
    };

    let mut merged = existing.clone();
    for key in OWNED_KEYS {
        match ours.get(*key) {
            Some(value) => {
                merged.insert((*key).to_string(), value.clone());
            }
            // A client whose entry omits `type` should not inherit a stale one.
            None => {
                merged.remove(*key);
            }
        }
    }
    Value::Object(merged)
}

/// Append the brain convention to an agent doc when a brain exists.
///
/// Clients without lifecycle hooks can only be *asked* to call `brain_load` and
/// `brain_flush`, so the ask goes in the same marked block as everything else —
/// it appears and disappears with the brain, and never duplicates.
fn with_brain_convention(base: &str) -> String {
    if !crate::brain::exists() {
        return base.to_string();
    }
    format!("{}\n\n{}", base.trim_end(), crate::brain::hooks::CONVENTION)
}

// ─── Agent markdown block ──────────────────────────────────────────────────────

/// Markers around the ragpilot section of an agent markdown file. Everything
/// between them belongs to ragpilot and is rewritten on each `init`; everything
/// outside is the user's and is never touched.
pub const BLOCK_START: &str = "<!-- ragpilot:start -->";
pub const BLOCK_END: &str = "<!-- ragpilot:end -->";

/// Write the ragpilot instructions into an agent markdown file (`CLAUDE.md`,
/// `AGENTS.md`) without ever duplicating them:
///
/// * no file — create it holding just the marked block;
/// * marked block — replace it in place;
/// * unmarked file that is byte-for-byte the doc an older ragpilot wrote —
///   upgrade it to the marked form;
/// * any other file — append the block, leaving the user's text alone.
fn upsert_doc(path: &Path, body: &str, display: &str) -> Result<()> {
    let block = format!("{BLOCK_START}\n{}\n{BLOCK_END}\n", body.trim_end_matches('\n'));

    if !path.exists() {
        std::fs::write(path, &block)?;
        println!("{} {}", "✓".green(), display);
        return Ok(());
    }

    let existing = std::fs::read_to_string(path)?;
    let updated = match block_span(&existing) {
        Some((start, end)) => {
            format!("{}{}{}", &existing[..start], block.trim_end_matches('\n'), &existing[end..])
        }
        // Written by a pre-marker ragpilot and never edited since: replace it
        // wholesale rather than appending a second copy of the same text.
        None if existing.trim_end() == body.trim_end() => block.clone(),
        None => {
            if existing.contains(BLOCK_START) || existing.contains(BLOCK_END) {
                println!(
                    "{} {} has an unterminated ragpilot marker — appending a fresh block; \
                     delete the stray marker to tidy up.",
                    "!".yellow(),
                    display
                );
            }
            let mut out = existing.clone();
            if !out.ends_with('\n') { out.push('\n'); }
            out.push('\n');
            out.push_str(&block);
            out
        }
    };

    if updated == existing {
        println!("{} {} (ragpilot block already current)", "i".blue(), display);
        return Ok(());
    }
    std::fs::write(path, updated)?;
    println!("{} {} (ragpilot block updated)", "✓".green(), display);
    Ok(())
}

/// The heading that opens the doc ragpilot ships. Text above the block that
/// starts with this is an older copy of our own doc, not the user's writing.
const DOC_SENTINEL: &str = "# AGENT EXECUTION POLICY";

/// Share of the leading text that must already appear inside the block before
/// it can be called redundant. Not 100%: a copy from an earlier release differs
/// by whatever we have since changed — the real files differ by exactly the one
/// line a fix in 0.6.0 rewrote.
const REDUNDANT_THRESHOLD: f64 = 0.9;

/// Text above the block that the block already says, and the lines it does not.
pub struct Redundant {
    /// Bytes that would be removed.
    pub bytes: usize,
    /// Substantive lines that appear above but NOT inside the block. These are
    /// what a tidy would actually lose, so they are shown before anything is
    /// deleted.
    pub lost: Vec<String>,
}

/// Whether the text above the ragpilot block is an older copy of the same doc.
///
/// Answers `None` for a file with no block, no leading text, or leading text
/// that is the user's own writing — those are never touched.
pub fn redundant_preamble(text: &str) -> Option<Redundant> {
    let start = text.find(BLOCK_START)?;
    let (top, block) = (&text[..start], &text[start..]);
    if !top.trim_start().starts_with(DOC_SENTINEL) {
        return None;
    }

    let block_lines: std::collections::HashSet<String> = substantive(block).into_iter().collect();
    let top_lines = substantive(top);
    if top_lines.is_empty() {
        return None;
    }

    let lost: Vec<String> = top_lines.iter().filter(|l| !block_lines.contains(*l)).cloned().collect();
    let overlap = 1.0 - (lost.len() as f64 / top_lines.len() as f64);
    (overlap >= REDUNDANT_THRESHOLD).then_some(Redundant { bytes: top.len(), lost })
}

/// Remove the redundant preamble, leaving the block as the whole file.
pub fn drop_preamble(path: &Path) -> Result<usize> {
    let text = std::fs::read_to_string(path)?;
    let Some(found) = redundant_preamble(&text) else { return Ok(0) };
    let start = text.find(BLOCK_START).expect("checked above");
    std::fs::write(path, &text[start..])?;
    Ok(found.bytes)
}

/// Lines worth comparing: whitespace collapsed, comments dropped, and anything
/// too short to distinguish one document from another ignored.
fn substantive(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| l.len() > 12 && !l.starts_with("<!--"))
        .collect()
}

/// Byte range of a complete `start … end` marker pair, if there is one.
fn block_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_START)?;
    let end = text[start..].find(BLOCK_END)? + start + BLOCK_END.len();
    Some((start, end))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BODY: &str = "## RagPilot\n- Call `rag_search` first.";

    fn scratch_file(label: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-agents-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("AGENTS.md")
    }

    #[test]
    fn creates_the_file_with_a_marked_block() {
        let path = scratch_file("create");
        upsert_doc(&path, BODY, "AGENTS.md").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(BLOCK_START));
        assert!(text.trim_end().ends_with(BLOCK_END));
        assert!(text.contains("rag_search"));

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn rerunning_never_duplicates_the_block() {
        let path = scratch_file("idempotent");
        upsert_doc(&path, BODY, "AGENTS.md").unwrap();
        let once = std::fs::read_to_string(&path).unwrap();

        upsert_doc(&path, BODY, "AGENTS.md").unwrap();
        upsert_doc(&path, BODY, "AGENTS.md").unwrap();

        let thrice = std::fs::read_to_string(&path).unwrap();
        assert_eq!(once, thrice);
        assert_eq!(thrice.matches(BLOCK_START).count(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn keeps_user_content_and_updates_only_the_block() {
        let path = scratch_file("user-content");
        std::fs::write(&path, "# My rules\n\nNever force-push.\n").unwrap();

        upsert_doc(&path, BODY, "AGENTS.md").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# My rules"));
        assert!(text.contains("Never force-push."));
        assert!(text.contains(BLOCK_START));

        // A changed body rewrites the block in place, user text untouched.
        upsert_doc(&path, "## RagPilot\n- Call `context_bundle` first.", "AGENTS.md").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Never force-push."));
        assert!(text.contains("context_bundle"));
        assert!(!text.contains("rag_search"));
        assert_eq!(text.matches(BLOCK_START).count(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn upgrades_an_unmarked_doc_written_by_an_older_ragpilot() {
        let path = scratch_file("upgrade");
        std::fs::write(&path, format!("{BODY}\n")).unwrap();

        upsert_doc(&path, BODY, "AGENTS.md").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(BLOCK_START));
        // The policy text appears exactly once — not appended beside itself.
        assert_eq!(text.matches("rag_search").count(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn block_span_needs_both_markers_in_order() {
        assert!(block_span("nothing here").is_none());
        assert!(block_span(&format!("{BLOCK_END} stray")).is_none());
        assert!(block_span(&format!("{BLOCK_START} unterminated")).is_none());

        let text = format!("a{BLOCK_START}b{BLOCK_END}c");
        let (s, e) = block_span(&text).unwrap();
        assert_eq!(&text[s..e], format!("{BLOCK_START}b{BLOCK_END}"));
    }

    #[test]
    fn project_and_global_client_lists_stay_disjoint() {
        for client in PROJECT_CLIENTS {
            assert!(!GLOBAL_CLIENTS.contains(client), "{client} is in both lists");
        }
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn re_registering_keeps_what_the_user_put_in_the_entry() {
        // The shape that broke: a dev machine carrying reporting credentials.
        let existing = json!({
            "type": "stdio",
            "command": "ragpilot",
            "args": ["--mcp-server"],
            "env": { "RAGPILOT_ENT_API": "http://127.0.0.1:8787", "RAGPILOT_ENT_AUDIT_ACTIONS": "1" },
            "timeout": 30
        });
        let merged = merge_entry(Some(&existing), &server_entry(true));

        assert_eq!(merged["env"]["RAGPILOT_ENT_API"], "http://127.0.0.1:8787");
        assert_eq!(merged["timeout"], 30);
        // …and the launch keys are still ours.
        assert_eq!(merged["command"], "ragpilot");
        assert_eq!(merged["args"], json!(["--mcp-server"]));
    }

    #[test]
    fn the_launch_keys_are_overwritten_not_merged() {
        let stale = json!({ "command": "rag", "args": ["--old-flag"], "env": { "KEEP": "1" } });
        let merged = merge_entry(Some(&stale), &server_entry(true));

        assert_eq!(merged["command"], "ragpilot");
        assert_eq!(merged["args"], json!(["--mcp-server"]));
        assert_eq!(merged["env"]["KEEP"], "1");
    }

    #[test]
    fn a_client_without_type_does_not_inherit_a_stale_one() {
        let existing = json!({ "type": "stdio", "command": "ragpilot", "env": { "KEEP": "1" } });
        // cursor's entry carries no `type`.
        let merged = merge_entry(Some(&existing), &server_entry(false));

        assert!(merged.get("type").is_none(), "{merged}");
        assert_eq!(merged["env"]["KEEP"], "1");
    }

    #[test]
    fn nothing_registered_yet_is_just_our_entry() {
        assert_eq!(merge_entry(None, &server_entry(true)), server_entry(true));
        // A non-object entry is replaced rather than merged into.
        assert_eq!(merge_entry(Some(&json!("garbage")), &server_entry(true)), server_entry(true));
    }
}

#[cfg(test)]
mod preamble_tests {
    use super::*;

    /// The shipped policy, as a body of distinguishable lines. Real files carry
    /// ~68 of these, which is why one changed line is far below the threshold.
    fn body(marker: &str) -> String {
        let mut out = String::from("# AGENT EXECUTION POLICY — RAG-FIRST\n");
        for i in 0..20 {
            out.push_str(&format!("Rule {i}: discovery goes through the MCP server, never a broad read.\n"));
        }
        out.push_str(&format!("It is registered in `{marker}` for this project.\n"));
        out
    }

    fn doc(top: &str) -> String {
        format!("{top}{}\n{}{}\n", BLOCK_START, body(".mcp.json"), BLOCK_END)
    }

    #[test]
    fn an_older_copy_of_our_own_doc_is_redundant() {
        // Differs by the one line a later release rewrote — the shape found
        // across nineteen real projects, where 67 of 68 lines matched.
        let old = format!("{}\n", body(".claude/settings.json"));
        let found = redundant_preamble(&doc(&old)).expect("should be redundant");

        assert_eq!(found.bytes, old.len());
        assert_eq!(found.lost.len(), 1, "{:?}", found.lost);
        assert!(found.lost[0].contains(".claude/settings.json"));
    }

    #[test]
    fn the_users_own_writing_is_never_redundant() {
        // A different opening heading: not our doc, never touched.
        let mine = "# Team rules\nNever force-push to main under any circumstances.\n\n";
        assert!(redundant_preamble(&doc(mine)).is_none());

        // Our sentinel, but the body has been rewritten — below the threshold.
        let mut rewritten = String::from("# AGENT EXECUTION POLICY — RAG-FIRST\n");
        for i in 0..20 {
            rewritten.push_str(&format!("Our own rule {i}: prefer grep, ignore the index entirely.\n"));
        }
        assert!(redundant_preamble(&doc(&rewritten)).is_none());
    }

    #[test]
    fn a_file_with_nothing_above_the_block_is_left_alone() {
        assert!(redundant_preamble(&doc("")).is_none());
        assert!(redundant_preamble("no block at all here").is_none());
    }

    #[test]
    fn dropping_the_preamble_keeps_the_block_intact() {
        let dir = std::env::temp_dir().join(format!("ragpilot-preamble-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("CLAUDE.md");

        let old = format!("{}\n", body(".claude/settings.json"));
        std::fs::write(&path, doc(&old)).unwrap();

        assert_eq!(drop_preamble(&path).unwrap(), old.len());

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(BLOCK_START));
        assert!(after.contains("`.mcp.json`"));
        assert!(!after.contains("settings.json"), "the stale line survived");
        // Idempotent: nothing left to drop.
        assert_eq!(drop_preamble(&path).unwrap(), 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

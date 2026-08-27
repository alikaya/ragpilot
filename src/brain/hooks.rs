//! Wiring the brain into agents.
//!
//! Two mechanisms, one behaviour contract: **`brain_load` at the start of a
//! session, a summary and a flush at the end.**
//!
//! Claude Code can enforce it — its hooks run whether or not the agent decides
//! to cooperate. Every other client is asked to cooperate, through a block in
//! its agent markdown. When Codex and friends grow lifecycle hooks, they move
//! to the first mechanism and the block stays valid.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Map, Value};

/// The instruction block appended to an agent markdown file when a brain
/// exists. Same contract as the hooks, spelled out for a client that cannot
/// enforce it.
pub const CONVENTION: &str = "\
## RagPilot Brain

You have a second brain: a persistent memory that outlives this session and is
not tied to this repository.

- **First thing in a session**, call `brain_load` and take the returned context
  seriously — it holds who you are, what was left half-done and what was
  already decided.
- The moment something is **decided or learned**, call `brain_note` with
  `kind: \"decision\"`. Do not wait for the end of the session.
- When you are **corrected** — \"do not do it that way\", \"I want it like this\" —
  call `brain_note` with `kind: \"rule\"` and a `why`. Rules load at the start of
  every session, so the same correction never has to be made twice.
- **Before the session ends**, call `brain_flush` with a summary, the decisions
  made, what is still open, and anything you **finished** in `closed_threads`.
  Open work is carried across sessions until you close it, so close what is done
  or it will follow you around.
- If you notice a **previous session closed without a flush**, reconstruct what
  you can from the transcript or the repository, note it, and carry on — a gap
  in the log is worth filling late.
- `brain_search` finds anything recorded earlier. Use it before asking the user
  to repeat themselves.";

/// Claude Code hook events wired to ragpilot, and the command each one runs.
///
/// `SessionEnd` rather than `Stop`: `Stop` fires after every assistant turn,
/// which would append a session block per turn instead of per session.
/// `PreCompact` runs the same command so a long session's record reaches disk
/// even when the context window fills first.
const CLAUDE_HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "ragpilot brain session-start"),
    ("SessionEnd", "ragpilot brain session-end"),
    ("PreCompact", "ragpilot brain session-end"),
];

/// Where Claude Code hooks are written.
///
/// `settings.local.json`, not `settings.json`: the latter is committed, and a
/// brain is personal. A shared hook would make every teammate run
/// `ragpilot brain session-start` — into a brain they do not have.
pub const CLAUDE_SETTINGS: &str = ".claude/settings.local.json";

/// Install the session hooks for a project. Idempotent: an entry that is
/// already there is left alone, and the user's other hooks are untouched.
pub fn install_claude(root: &Path) -> Result<()> {
    let path = root.join(CLAUDE_SETTINGS);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: Map<String, Value> = match std::fs::read_to_string(&path) {
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw)
            .with_context(|| format!("Cannot parse {}", path.display()))?,
        _ => Map::new(),
    };

    let hooks = doc
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        anyhow::bail!("\"hooks\" in {} is not an object", path.display());
    };

    let mut added = Vec::new();
    for (event, command) in CLAUDE_HOOKS {
        let list = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = list.as_array_mut() else {
            anyhow::bail!("hooks.{event} in {} is not an array", path.display());
        };
        if list.iter().any(|entry| mentions_command(entry, command)) {
            continue;
        }
        list.push(json!({ "hooks": [{ "type": "command", "command": command }] }));
        added.push(*event);
    }

    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(doc))? + "\n")?;

    if added.is_empty() {
        println!("{} {} (hooks already installed)", "i".blue(), CLAUDE_SETTINGS);
    } else {
        println!("{} {} ({})", "✓".green(), CLAUDE_SETTINGS, added.join(", "));
    }
    Ok(())
}

/// Whether an existing hook entry already runs `command` — checked by content
/// so a hand-edited entry is recognised too.
fn mentions_command(entry: &Value, command: &str) -> bool {
    entry.to_string().contains(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-hooks-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(root: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(CLAUDE_SETTINGS)).unwrap()).unwrap()
    }

    #[test]
    fn installs_all_three_events() {
        let root = scratch("fresh");
        install_claude(&root).unwrap();

        let doc = settings(&root);
        for (event, command) in CLAUDE_HOOKS {
            let list = doc["hooks"][event].as_array().expect("event array");
            assert_eq!(list.len(), 1, "{event}");
            assert!(list[0].to_string().contains(command), "{event}");
        }
        // Stop is deliberately not wired — it fires every turn.
        assert!(doc["hooks"].get("Stop").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rerunning_adds_nothing_and_keeps_foreign_hooks() {
        let root = scratch("idempotent");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            root.join(CLAUDE_SETTINGS),
            r#"{
              "permissions": { "allow": ["Bash(ls:*)"] },
              "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }] }
            }"#,
        )
        .unwrap();

        install_claude(&root).unwrap();
        let once = std::fs::read_to_string(root.join(CLAUDE_SETTINGS)).unwrap();
        install_claude(&root).unwrap();
        let twice = std::fs::read_to_string(root.join(CLAUDE_SETTINGS)).unwrap();

        assert_eq!(once, twice, "a second install changed the file");

        let doc = settings(&root);
        // The user's own settings and hook survived…
        assert_eq!(doc["permissions"]["allow"][0], "Bash(ls:*)");
        let start = doc["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert!(start[0].to_string().contains("echo mine"));
        // …and ours was added exactly once.
        assert_eq!(
            twice.matches("ragpilot brain session-start").count(),
            1,
            "session-start hook duplicated"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_hand_written_equivalent_hook_is_recognised() {
        let root = scratch("handwritten");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            root.join(CLAUDE_SETTINGS),
            r#"{"hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"ragpilot brain session-start --max-tokens 2000"}]}]}}"#,
        )
        .unwrap();

        install_claude(&root).unwrap();

        let doc = settings(&root);
        assert_eq!(doc["hooks"]["SessionStart"].as_array().unwrap().len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_convention_states_the_same_contract_as_the_hooks() {
        for tool in ["brain_load", "brain_note", "brain_flush", "brain_search"] {
            assert!(CONVENTION.contains(tool), "{tool} missing from the convention");
        }
    }
}

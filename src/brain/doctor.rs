//! `ragpilot brain doctor` — check the vault, and repair what is safe to
//! repair.
//!
//! The split is deliberate: anything that only *derives* from the markdown
//! (the index, orphaned vectors) is fixed automatically, because it can always
//! be rebuilt. Anything that touches the markdown itself is reported with a
//! suggestion and left for the human.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;

use super::config::{BrainConfig, SCHEMA_VERSION};
use super::{engine, schedule};

pub struct Finding {
    pub ok: bool,
    pub label: String,
    pub detail: Option<String>,
}

impl Finding {
    fn ok(label: impl Into<String>) -> Self {
        Self { ok: true, label: label.into(), detail: None }
    }
    fn bad(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { ok: false, label: label.into(), detail: Some(detail.into()) }
    }
    fn with(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

pub async fn cmd_doctor(fix: bool) -> Result<()> {
    if !super::exists() {
        println!("{} No brain at {}.", "i".blue(), super::dir().display());
        println!("  Set one up with: {}", "ragpilot brain init".bold());
        return Ok(());
    }

    println!("{}", "─── ragpilot brain doctor ───────────────────────".bold());
    for finding in checks(fix).await {
        println!(
            "  {}  {}",
            if finding.ok { "✓".green() } else { "✗".red() },
            finding.label
        );
        if let Some(detail) = finding.detail {
            for line in detail.lines() {
                println!("       {}", line.dimmed());
            }
        }
    }
    Ok(())
}

/// The full check list. Shared with `ragpilot doctor`, which prints a summary.
pub async fn checks(fix: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    out.push(schema_check());
    out.push(engine_check());
    out.push(schedule_check());
    out.push(wikilink_check());
    out.push(index_check(fix).await);
    out.push(orphan_check(fix).await);
    out.push(git_check(fix));
    out
}

// ── individual checks ──────────────────────────────────────────────────────

fn schema_check() -> Finding {
    match BrainConfig::load(&super::config_path()) {
        Ok(cfg) if cfg.brain.schema_version == SCHEMA_VERSION => {
            Finding::ok(format!("Schema v{SCHEMA_VERSION}"))
        }
        Ok(cfg) if cfg.brain.schema_version < SCHEMA_VERSION => Finding::bad(
            format!("Schema v{} (expected v{SCHEMA_VERSION})", cfg.brain.schema_version),
            "Run `ragpilot brain init` — it upgrades in place and changes nothing else.",
        ),
        Ok(cfg) => Finding::bad(
            format!("Schema v{} is newer than this binary (v{SCHEMA_VERSION})", cfg.brain.schema_version),
            "Upgrade ragpilot rather than downgrading the brain.",
        ),
        Err(e) => Finding::bad("Config unreadable", e.to_string()),
    }
}

fn engine_check() -> Finding {
    let Ok(cfg) = BrainConfig::load(&super::config_path()) else {
        return Finding::bad("Compiler engine", "config unreadable");
    };
    match engine::create(&cfg, None) {
        Ok(e) => match e.available() {
            Ok(()) => Finding::ok(format!("Compiler engine '{}'", e.name())),
            Err(why) => Finding::bad(format!("Compiler engine '{}'", e.name()), why.to_string()),
        },
        Err(why) => Finding::bad("Compiler engine", why.to_string()),
    }
}

fn schedule_check() -> Finding {
    let configured = BrainConfig::load(&super::config_path())
        .map(|c| c.compiler.schedule)
        .unwrap_or_default();

    if configured.trim().is_empty() {
        return Finding::ok("Scheduler (manual only)")
            .with("compiler.schedule is empty — compile with `ragpilot brain compile`.");
    }
    if schedule::installed() {
        Finding::ok(format!("Scheduler installed ({configured} daily)"))
    } else {
        Finding::bad(
            format!("Scheduler not installed (configured for {configured})"),
            "Install with `ragpilot brain schedule --install`.",
        )
    }
}

/// `[[links]]` that point at nothing. A near-match is suggested; nothing is
/// rewritten, because a wrong auto-correction is worse than a dangling link.
fn wikilink_check() -> Finding {
    let notes = existing_slugs();
    let mut broken: Vec<String> = Vec::new();

    for (file, text) in vault_markdown() {
        for link in wikilinks(&text) {
            if notes.contains_key(&link) {
                continue;
            }
            let suggestion = closest(&link, &notes);
            broken.push(match suggestion {
                Some(s) => format!("{file}: [[{link}]] → did you mean [[{s}]]?"),
                None => format!("{file}: [[{link}]] has no target"),
            });
        }
    }

    if broken.is_empty() {
        Finding::ok("Wikilinks all resolve")
    } else {
        let count = broken.len();
        broken.truncate(10);
        Finding::bad(format!("{count} broken wikilink(s)"), broken.join("\n"))
    }
}

/// Markdown that changed since it was indexed. Always safe to repair: the
/// index is derived from the markdown, never the other way round.
async fn index_check(fix: bool) -> Finding {
    let Ok(rt) = super::runtime::runtime().await else {
        return Finding::bad("Index", "cannot open the brain index");
    };

    // Drift is markdown that no longer matches what was indexed from it.
    let state_path = crate::paths::ProjectPaths::brain().state();
    let state = crate::indexer::IndexState::load(&state_path).unwrap_or_default();
    let root = super::dir();
    let dirty = state
        .file_hashes
        .iter()
        .filter(|(rel, stored)| match std::fs::read_to_string(root.join(rel.as_str())) {
            Ok(text) => &crate::indexer::compute_hash(&text) != *stored,
            Err(_) => false, // a missing file is an orphan, not drift
        })
        .count();

    if dirty == 0 {
        return Finding::ok("Index up to date");
    }
    if !fix {
        return Finding::bad(
            format!("{dirty} file(s) changed since indexing"),
            "Re-index with `ragpilot brain index` (or run doctor with --fix).",
        );
    }
    match rt.orchestrator.ensure_index(false).await {
        Ok(r) => Finding::ok(format!("Index repaired ({} file(s) re-indexed)", r.indexed)),
        Err(e) => Finding::bad("Index repair failed", e.to_string()),
    }
}

/// Vectors for markdown that no longer exists. Also derived, also safe.
async fn orphan_check(fix: bool) -> Finding {
    let Ok(rt) = super::runtime::runtime().await else {
        return Finding::bad("Orphan vectors", "cannot open the brain index");
    };
    let state = match crate::indexer::IndexState::load(&crate::paths::ProjectPaths::brain().state()) {
        Ok(s) => s,
        Err(e) => return Finding::bad("Orphan vectors", e.to_string()),
    };

    let root = super::dir();
    let orphans: Vec<String> = state
        .file_hashes
        .keys()
        .filter(|rel| !root.join(rel).exists())
        .cloned()
        .collect();

    if orphans.is_empty() {
        return Finding::ok("No orphan vectors");
    }
    if !fix {
        return Finding::bad(
            format!("{} orphan vector set(s)", orphans.len()),
            format!("{}\nClear them with --fix.", orphans.join("\n")),
        );
    }

    let mut cleared = 0;
    for rel in &orphans {
        if rt.orchestrator.vector_store.delete_by_source(rel).await.is_ok() {
            cleared += 1;
        }
    }
    // The state must forget them too, or they come back next run.
    let mut state = state;
    state.file_hashes.retain(|rel, _| root.join(rel).exists());
    let _ = state.save(&crate::paths::ProjectPaths::brain().state());

    Finding::ok(format!("Cleared {cleared} orphan vector set(s)"))
}

fn git_check(fix: bool) -> Finding {
    let root = super::dir();
    if !root.join(".git").exists() {
        return Finding::bad("Git", "The vault is not a git repo — compiles cannot be reverted.");
    }

    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&root)
        .output();
    let Ok(output) = output else {
        return Finding::bad("Git", "git is not available");
    };
    let pending: Vec<&str> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| Box::leak(l.to_string().into_boxed_str()) as &str)
        .collect();

    if pending.is_empty() {
        return Finding::ok("Git clean");
    }
    if !fix {
        let mut listed: Vec<String> = pending.iter().take(10).map(|s| s.to_string()).collect();
        listed.push("Commit them with --fix.".into());
        return Finding::bad(format!("{} uncommitted change(s)", pending.len()), listed.join("\n"));
    }

    let ok = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && std::process::Command::new("git")
            .args(["commit", "-q", "-m", &format!("doctor: {}", super::vault::today())])
            .current_dir(&root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if ok {
        Finding::ok(format!("Committed {} pending change(s)", pending.len()))
    } else {
        Finding::bad("Git commit failed", "Commit the vault by hand.")
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Every markdown file in the vault that a link could point at, as
/// `slug → path`.
fn existing_slugs() -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    for dir in [super::knowledge_dir(), super::skills_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                if let Some(stem) = path.file_stem() {
                    out.insert(stem.to_string_lossy().to_string(), path);
                }
            }
        }
    }
    out
}

/// Markdown across the linkable parts of the vault, as `relative path → text`.
fn vault_markdown() -> Vec<(String, String)> {
    let root = super::dir();
    let mut out = Vec::new();
    for dir in [super::knowledge_dir(), super::skills_dir(), super::daily_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().to_string();
                out.push((rel, text));
            }
        }
    }
    out
}

/// Every `[[target]]` in a document.
pub fn wikilinks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let link = after[..end].trim();
        if !link.is_empty() && !link.contains('\n') {
            out.push(link.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

/// The nearest existing slug, if one is close enough to be worth suggesting.
fn closest<'a>(link: &str, notes: &'a BTreeMap<String, PathBuf>) -> Option<&'a str> {
    let threshold = (link.len() / 3).max(1);
    notes
        .keys()
        .map(|slug| (edit_distance(link, slug), slug.as_str()))
        .filter(|(d, _)| *d <= threshold)
        .min_by_key(|(d, _)| *d)
        .map(|(_, slug)| slug)
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilinks_are_extracted_and_junk_is_ignored() {
        let text = "See [[alpha]] and [[beta-note]].\nAn [[ unterminated\nAnd [[]] empty.";
        assert_eq!(wikilinks(text), vec!["alpha", "beta-note"]);

        assert!(wikilinks("no links here").is_empty());
        assert_eq!(wikilinks("[[ spaced ]]"), vec!["spaced"]);
    }

    #[test]
    fn edit_distance_is_symmetric_and_correct() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), edit_distance("sitting", "kitten"));
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn a_near_miss_is_suggested_and_a_wild_miss_is_not() {
        let mut notes = BTreeMap::new();
        notes.insert("qdrant-alias-migration".to_string(), PathBuf::from("x"));
        notes.insert("project-registry".to_string(), PathBuf::from("y"));

        assert_eq!(closest("qdrant-alias-migraton", &notes), Some("qdrant-alias-migration"));
        assert_eq!(closest("project-registy", &notes), Some("project-registry"));
        // Nothing remotely similar: silence beats a bad guess.
        assert_eq!(closest("cooking-recipes", &notes), None);
    }
}

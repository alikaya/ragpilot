//! Semantic diff: turn a `git diff` into symbol-level changes plus their blast
//! radius. Instead of reporting changed *lines*, it parses the old and new
//! versions of each changed file, diffs them by symbol, and classifies each as
//! added / removed / signature-changed / modified. For impactful changes it
//! then pulls callers (from the symbol graph) and dependent files (from the
//! impact index) so an agent can say "X's return type changed → affects Y, Z".
//!
//! Rust gets exact signatures via tree-sitter; other languages fall back to the
//! declaration line (so added/removed and decl-line changes are still caught).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::parser::{Parser, RegexParser, SymbolDetail};
use crate::store::impact_index::ImpactIndexStore;
use crate::store::symbol_graph::SymbolGraphStore;

#[derive(Debug, Serialize)]
pub struct CallerRef {
    pub symbol: String,
    pub path:   String,
    pub line:   usize,
}

#[derive(Debug, Serialize)]
pub struct SymbolChange {
    pub symbol: String,
    pub kind:   String,
    pub path:   String,
    /// "added" | "removed" | "signature_changed" | "modified"
    pub change: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after:  Option<String>,
    pub callers:        Vec<CallerRef>,
    pub affected_files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub target:        String,
    pub files_changed: usize,
    pub changes:       Vec<SymbolChange>,
    pub summary:       Vec<String>,
}

/// How many callers / affected files to list per change before truncating.
const MAX_REFS: usize = 25;

struct Target {
    /// Base ref to diff *from* (the "before").
    base: String,
    /// Ref to diff *to*; `None` means the working tree.
    new_ref: Option<String>,
}

fn parse_target(arg: Option<&str>) -> Target {
    match arg {
        None => Target { base: "HEAD".into(), new_ref: None },
        Some(a) if a.contains("..") => {
            let mut it = a.splitn(2, "..");
            let base = it.next().unwrap_or("HEAD").to_string();
            let right = it.next().unwrap_or("").to_string();
            Target { base, new_ref: if right.is_empty() { None } else { Some(right) } }
        }
        Some(a) => Target { base: a.to_string(), new_ref: None },
    }
}

/// Analyze the semantic diff for `target` (e.g. None, "HEAD~1", "main..HEAD").
pub async fn analyze(root: &Path, target_arg: Option<&str>) -> Result<DiffReport> {
    let target = parse_target(target_arg);
    let label = match &target.new_ref {
        Some(r) => format!("{}..{}", target.base, r),
        None => format!("{} (working tree)", target.base),
    };

    let db_path = Config::stores_db(root);
    let sg = SymbolGraphStore::new(db_path.clone());
    let impact = ImpactIndexStore::new(db_path);

    let files = changed_files(root, &target)?;
    let files_changed = files.len();

    let mut changes: Vec<SymbolChange> = Vec::new();

    for (status, path) in &files {
        let language = lang_of(path);
        if language.is_empty() {
            continue; // non-source / unknown
        }

        let old_src = if *status == b'A' { String::new() } else { git_show(root, &target.base, path) };
        let new_src = match &target.new_ref {
            _ if *status == b'D' => String::new(),
            Some(r) => git_show(root, r, path),
            None => std::fs::read_to_string(root.join(path)).unwrap_or_default(),
        };

        let old = details(path, &old_src, language);
        let new = details(path, &new_src, language);

        for ch in diff_symbols(path, &old, &new) {
            changes.push(ch);
        }
    }

    // Blast radius for impactful changes (everything except pure additions).
    for ch in &mut changes {
        if ch.change == "added" {
            continue;
        }
        if let Ok(callers) = sg.callers(&ch.symbol).await {
            let mut seen = HashSet::new();
            for c in callers {
                let (cpath, csym) = split_caller_id(&c.caller_id);
                // Don't list the symbol calling itself within its own file.
                if csym == ch.symbol && cpath == ch.path {
                    continue;
                }
                if seen.insert((cpath.clone(), csym.clone())) {
                    ch.callers.push(CallerRef { symbol: csym, path: cpath, line: c.call_line });
                }
            }
            ch.callers.truncate(MAX_REFS);
        }
        if let Ok(mut affected) = impact.get_affected(&[ch.path.clone()]).await {
            affected.retain(|p| p != &ch.path);
            affected.sort();
            affected.truncate(MAX_REFS);
            ch.affected_files = affected;
        }
    }

    let summary = build_summary(&changes);

    Ok(DiffReport { target: label, files_changed, changes, summary })
}

fn diff_symbols(path: &str, old: &[SymbolDetail], new: &[SymbolDetail]) -> Vec<SymbolChange> {
    let old_map: HashMap<&str, &SymbolDetail> = old.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_map: HashMap<&str, &SymbolDetail> = new.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut out = Vec::new();

    for (name, n) in &new_map {
        match old_map.get(name) {
            None => out.push(change(path, n, "added", None, Some(n.signature.clone()))),
            Some(o) => {
                if o.signature != n.signature {
                    out.push(change(path, n, "signature_changed",
                        Some(o.signature.clone()), Some(n.signature.clone())));
                } else if !n.body.is_empty() && o.body != n.body {
                    out.push(change(path, n, "modified", None, None));
                }
            }
        }
    }
    for (name, o) in &old_map {
        if !new_map.contains_key(name) {
            out.push(change(path, o, "removed", Some(o.signature.clone()), None));
        }
    }

    out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    out
}

fn change(path: &str, d: &SymbolDetail, kind: &str, before: Option<String>, after: Option<String>) -> SymbolChange {
    SymbolChange {
        symbol: d.name.clone(),
        kind:   d.kind.clone(),
        path:   path.to_string(),
        change: kind.to_string(),
        before,
        after,
        callers: Vec::new(),
        affected_files: Vec::new(),
    }
}

/// Per-symbol details for a file's content: tree-sitter for Rust, regex
/// (declaration-line signature) for everything else.
fn details(path: &str, content: &str, language: &str) -> Vec<SymbolDetail> {
    if content.is_empty() {
        return Vec::new();
    }
    if language == "rust" {
        if let Some(d) = crate::parser::tree_sitter_parser::rust_details(content) {
            return d;
        }
    }
    // Fallback: declaration line as the signature, no body.
    let lines: Vec<&str> = content.lines().collect();
    RegexParser
        .parse(path, content, language)
        .symbols
        .into_iter()
        .map(|s| {
            let sig = lines
                .get(s.start_line.saturating_sub(1))
                .map(|l| l.trim().trim_end_matches('{').trim().to_string())
                .unwrap_or_default();
            SymbolDetail { name: s.name, kind: s.kind, signature: sig, body: String::new(), start_line: s.start_line }
        })
        .collect()
}

fn build_summary(changes: &[SymbolChange]) -> Vec<String> {
    let mut out = Vec::new();
    for c in changes {
        let affected = c.callers.len();
        let line = match c.change.as_str() {
            "signature_changed" => format!(
                "⚠ {}::{} signature changed: `{}` → `{}`{}",
                c.path, c.symbol,
                c.before.as_deref().unwrap_or(""),
                c.after.as_deref().unwrap_or(""),
                impact_suffix(affected, &c.callers),
            ),
            "removed" => format!(
                "✗ {}::{} removed{}",
                c.path, c.symbol, impact_suffix(affected, &c.callers),
            ),
            "modified" => format!(
                "~ {}::{} body changed{}",
                c.path, c.symbol, impact_suffix(affected, &c.callers),
            ),
            "added" => format!("+ {}::{} added ({})", c.path, c.symbol, c.kind),
            _ => continue,
        };
        out.push(line);
    }
    out
}

fn impact_suffix(n: usize, callers: &[CallerRef]) -> String {
    if n == 0 {
        return " — no known callers".to_string();
    }
    let names: Vec<String> = callers.iter().take(5).map(|c| format!("{}::{}", c.path, c.symbol)).collect();
    let more = if n > 5 { format!(", +{} more", n - 5) } else { String::new() };
    format!(" — affects {n} caller(s): {}{more}", names.join(", "))
}

// ─── git plumbing ───────────────────────────────────────────────────────────────

fn changed_files(root: &Path, target: &Target) -> Result<Vec<(u8, String)>> {
    let spec = match &target.new_ref {
        Some(r) => format!("{}..{}", target.base, r),
        None => target.base.clone(),
    };
    let out = Command::new("git")
        .current_dir(root)
        .args(["diff", "--name-status", "--no-renames", &spec])
        .output()
        .context("failed to run git diff")?;
    if !out.status.success() {
        anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or("").bytes().next().unwrap_or(b'M');
        if let Some(path) = parts.next() {
            files.push((status, path.to_string()));
        }
    }
    Ok(files)
}

fn git_show(root: &Path, rev: &str, path: &str) -> String {
    Command::new("git")
        .current_dir(root)
        .arg("show")
        .arg(format!("{rev}:{path}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn lang_of(path: &str) -> &'static str {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    match crate::indexer::file_language(ext) {
        // Only languages our parsers understand structurally.
        l @ ("rust" | "python" | "javascript" | "typescript" | "go" | "java"
            | "kotlin" | "scala" | "ruby" | "php" | "swift") => l,
        _ => "",
    }
}

fn split_caller_id(id: &str) -> (String, String) {
    match id.rsplit_once("::") {
        Some((p, s)) => (p.to_string(), s.to_string()),
        None => (String::new(), id.to_string()),
    }
}

/// Render a report as colorized text for the CLI.
pub fn render(report: &DiffReport) -> String {
    use colored::Colorize;
    let mut s = String::new();
    s.push_str(&format!("{}\n", "─── Semantic Diff ───────────────────────".bold()));
    s.push_str(&format!("  Target:        {}\n", report.target));
    s.push_str(&format!("  Files changed: {}\n", report.files_changed));
    s.push_str(&format!("  Symbol changes: {}\n", report.changes.len()));

    if report.summary.is_empty() {
        s.push_str(&format!("\n  {}\n", "No symbol-level changes detected.".dimmed()));
        return s;
    }

    s.push('\n');
    for line in &report.summary {
        let colored = if line.starts_with('⚠') {
            line.yellow().to_string()
        } else if line.starts_with('✗') {
            line.red().to_string()
        } else if line.starts_with('+') {
            line.green().to_string()
        } else {
            line.normal().to_string()
        };
        s.push_str(&format!("  {colored}\n"));
        // List affected files under the impactful changes.
    }

    let affected: Vec<&String> = report.changes.iter().flat_map(|c| &c.affected_files).collect();
    if !affected.is_empty() {
        let uniq: HashSet<&String> = affected.into_iter().collect();
        let mut list: Vec<&String> = uniq.into_iter().collect();
        list.sort();
        s.push_str(&format!("\n  {}\n", "Dependent files (import graph):".bold()));
        for f in list {
            s.push_str(&format!("    {f}\n"));
        }
    }
    s
}

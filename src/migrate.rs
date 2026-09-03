//! `ragpilot migrate` and `ragpilot projects` (Phase 4).
//!
//! Everything that makes the move from a project-local `.rag/` to the global
//! data root safe and reversible-by-inspection: the data files are moved, the
//! project is registered, and the existing Qdrant index is *aliased* to its new
//! name rather than rebuilt — migrating is a rename, not a re-embed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::paths::{self, ProjectPaths, Registry};

/// Files and directories that make up a project's data.
const MOVABLE: &[&str] = &["config.toml", "state.json", "stores.db", "queries"];

// ── migrate ────────────────────────────────────────────────────────────────

pub async fn cmd_migrate(keep: bool, yes: bool) -> Result<()> {
    let root = paths::canonical(&std::env::current_dir()?)?;
    migrate_one(&root, keep, yes).await
}

/// Migrate one project. `yes` skips the `.rag/` deletion prompt — bulk runs
/// answer it once, up front, rather than per project.
async fn migrate_one(root: &Path, keep: bool, yes: bool) -> Result<()> {
    let root = root.to_path_buf();
    let legacy = ProjectPaths::legacy(&root);

    if !legacy.config().exists() {
        println!(
            "{} No .rag/config.toml in {} — nothing to migrate.",
            "i".blue(),
            root.display()
        );
        return Ok(());
    }

    let mut registry = Registry::load()?;
    if let Some(entry) = registry.lookup(&root) {
        println!(
            "{} Already registered as '{}' — its data is at {}.",
            "i".blue(),
            entry.id,
            paths::project_dir(&entry.id).display()
        );
        println!("  The leftover .rag/ can be deleted by hand once you are happy.");
        return Ok(());
    }

    let target = ProjectPaths::global(&root);
    let id = target.id().unwrap_or_default().to_string();

    // Never overwrite: a half-finished earlier attempt must be resolved by the
    // user, not silently clobbered.
    for name in MOVABLE {
        let to = target.data_dir().join(name);
        if to.exists() {
            anyhow::bail!(
                "{} already exists. Remove it (or the whole directory) and re-run migrate.",
                to.display()
            );
        }
    }

    println!("{} Migrating {} → {}", "→".cyan(), root.display(), target.data_dir().display());
    std::fs::create_dir_all(target.data_dir())?;

    // `--keep` copies instead of moving, so the old `.rag/` stays a complete,
    // working fallback the user can roll back to by hand.
    let mut moved = Vec::new();
    for name in MOVABLE {
        let from = legacy.data_dir().join(name);
        if !from.exists() {
            continue;
        }
        let to = target.data_dir().join(name);
        if keep {
            copy_path(&from, &to).with_context(|| format!("Cannot copy {}", from.display()))?;
        } else {
            move_path(&from, &to).with_context(|| format!("Cannot move {}", from.display()))?;
        }
        moved.push(*name);
    }
    println!(
        "{} {}: {}",
        "✓".green(),
        if keep { "Copied" } else { "Moved" },
        moved.join(", ")
    );

    registry.upsert(&root);
    registry.save()?;
    println!("{} Registered as '{}'", "✓".green(), id.bold());

    match alias_existing_index(&target, &id).await {
        Ok(Some(message)) => println!("{} {message}", "✓".green()),
        Ok(None) => {}
        Err(e) => {
            println!("{} Index could not be re-pointed: {e}", "!".yellow());
            println!("  Run `ragpilot init --force` to rebuild it under the new name.");
        }
    }

    remove_legacy_dir(&legacy, keep, yes)?;

    println!("\n{} Migration complete. Verify with: {}", "✓".green(), "ragpilot doctor".bold());
    Ok(())
}

/// Point the project's new collection name at the collection it already has.
///
/// Old configs pin `collection = "<project name>"`, which would keep the old
/// name in force forever, so that default pin is dropped first. A *custom*
/// name the user chose is left alone — there is nothing to rename.
///
/// Returns a line to print, or `None` when nothing needed doing.
async fn alias_existing_index(target: &ProjectPaths, id: &str) -> Result<Option<String>> {
    let config_path = target.config();
    let config = Config::load(&config_path)?;
    let project_name = config.project.name.clone();
    let default_pin = paths::normalize_collection(&project_name);

    let pinned = config.qdrant.collection.clone();
    if let Some(name) = &pinned {
        if paths::normalize_collection(name) != default_pin {
            return Ok(Some(format!(
                "Collection '{name}' is pinned in the config — left as it is."
            )));
        }
        unpin_collection(&config_path)?;
    }

    let store = crate::store::qdrant::QdrantStore::new(&config.qdrant)?;
    if !store.exists(&default_pin).await {
        // Nothing indexed under the old name — the next index run creates the
        // collection under the id, which is exactly what we want.
        return Ok(None);
    }
    if store.exists(id).await {
        return Ok(Some(format!("Collection '{id}' already exists — left as it is.")));
    }

    // Under the old scheme the collection was named after the *folder*, so two
    // projects with the same folder name in different places shared one. If
    // another project already claimed this index, aliasing onto it would make
    // them silently share a single index from now on.
    let claimed = store.aliases_of(&default_pin).await;
    if let Some(other) = claimed.iter().find(|a| a.as_str() != id) {
        return Ok(Some(format!(
            "Collection '{default_pin}' is already claimed by '{other}' — two projects share \
             this legacy index. Left alone; run `ragpilot init --force` here to build '{id}' \
             from scratch."
        )));
    }

    store.create_alias_for(&default_pin, id).await?;
    Ok(Some(format!(
        "Index kept: '{default_pin}' is now reachable as '{id}' (alias, no re-index)"
    )))
}

/// Remove `collection = "…"` from a config file, leaving every other line —
/// comments and formatting included — exactly as the user had it.
fn unpin_collection(config_path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(config_path)?;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("collection"))
        .collect();
    let mut out = kept.join("\n");
    out.push('\n');
    std::fs::write(config_path, out)?;
    Ok(())
}

fn remove_legacy_dir(legacy: &ProjectPaths, keep: bool, yes: bool) -> Result<()> {
    let dir = legacy.data_dir();
    if keep {
        println!("{} Kept {} as a fallback (--keep)", "i".blue(), dir.display());
        return Ok(());
    }
    if !yes && !confirm(&format!("Delete {}?", dir.display()))? {
        println!("{} Kept {}", "i".blue(), dir.display());
        return Ok(());
    }
    std::fs::remove_dir_all(dir)
        .with_context(|| format!("Cannot remove {}", dir.display()))?;
    println!("{} Removed {}", "✓".green(), dir.display());
    Ok(())
}

// ── scan / bulk migrate ────────────────────────────────────────────────────

/// Directories never worth descending into while hunting for legacy projects.
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "vendor", ".venv", "venv", "dist", "build",
    ".cache", ".next", ".nuxt", "__pycache__",
];

/// A legacy project found by the scanner.
pub struct Legacy {
    pub root: PathBuf,
    pub project_name: String,
    /// The collection its config points at under the old naming scheme.
    pub collection: String,
    pub id: String,
    pub registered: bool,
    pub size_kb: u64,
}

/// Walk `root` for `.rag/config.toml`. Nested projects are legitimate — a
/// monorepo can index a subdirectory separately — so the walk does not stop at
/// the first hit.
pub fn scan(root: &Path, registry: &Registry) -> Vec<Legacy> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".rag" {
                if let Some(legacy) = describe(&dir, registry) {
                    found.push(legacy);
                }
                continue;
            }
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            stack.push(path);
        }
    }
    found.sort_by(|a, b| a.root.cmp(&b.root));
    found
}

fn describe(project_root: &Path, registry: &Registry) -> Option<Legacy> {
    let legacy = ProjectPaths::legacy(project_root);
    if !legacy.config().exists() {
        return None;
    }
    let root = paths::canonical(project_root).ok()?;
    let config = Config::load(&legacy.config()).ok()?;
    let target = ProjectPaths::global(&root);

    Some(Legacy {
        project_name: config.project.name.clone(),
        collection: legacy.collection(config.qdrant.collection.as_deref(), &config.project.name),
        id: target.id().unwrap_or_default().to_string(),
        registered: registry.lookup(&root).is_some(),
        size_kb: dir_size(legacy.data_dir()) / 1024,
        root,
    })
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Legacy collection names claimed by more than one project. Under the old
/// scheme the name came from the folder, so `~/dev/api` and `~/tmp/api` are one
/// collection — migrating both would leave them sharing a single index.
fn collisions(found: &[Legacy]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut by_collection: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for legacy in found {
        by_collection
            .entry(legacy.collection.clone())
            .or_default()
            .push(legacy.root.clone());
    }
    by_collection.retain(|_, roots| roots.len() > 1);
    by_collection
}

pub async fn cmd_scan(root: &Path, migrate_all: bool, keep: bool, yes: bool) -> Result<()> {
    let root = paths::canonical(root)
        .with_context(|| format!("Cannot resolve {}", root.display()))?;
    let registry = Registry::load()?;

    println!("{} Scanning {} for legacy .rag/ projects…", "→".cyan(), root.display());
    let found = scan(&root, &registry);
    if found.is_empty() {
        println!("{} None found — nothing to migrate.", "✓".green());
        return Ok(());
    }

    let clashing = collisions(&found);
    let pending: Vec<&Legacy> = found.iter().filter(|l| !l.registered).collect();
    let total_kb: u64 = pending.iter().map(|l| l.size_kb).sum();

    println!("\n{}", "─── Legacy projects ─────────────────────────────".bold());
    for legacy in &found {
        let status = if legacy.registered {
            "already migrated".green()
        } else if clashing.contains_key(&legacy.collection) {
            "SHARED COLLECTION".yellow()
        } else {
            "to migrate".normal()
        };
        println!("  {} — {}", legacy.root.display(), status);
        println!(
            "      '{}': collection '{}' → '{}'  ({} KB)",
            legacy.project_name, legacy.collection, legacy.id, legacy.size_kb
        );
    }

    if !clashing.is_empty() {
        println!("\n{}", "─── Shared collections ──────────────────────────".bold());
        for line in [
            "The old naming scheme took the collection name from the folder, so these",
            "projects share one index. Migrating them together would leave them sharing",
            "it for good, so bulk migration skips them — migrate the one you want to",
            "keep the index for, then `ragpilot init --force` in the others.",
        ] {
            println!("  {line}");
        }
        for (collection, roots) in &clashing {
            println!("  {}:", collection.yellow());
            for r in roots {
                println!("      {}", r.display());
            }
        }
    }

    println!(
        "\n  {} project(s) found · {} to migrate · {} KB of project-local data",
        found.len(),
        pending.len(),
        total_kb
    );

    if !migrate_all {
        println!("\n  Migrate them with: {}", format!("ragpilot migrate --all {}", root.display()).bold());
        println!("  Or one at a time:  {}", "cd <project> && ragpilot migrate".bold());
        return Ok(());
    }

    let safe: Vec<&Legacy> = pending
        .iter()
        .copied()
        .filter(|l| !clashing.contains_key(&l.collection))
        .collect();
    if safe.is_empty() {
        println!("\n{} Nothing to migrate automatically.", "i".blue());
        return Ok(());
    }
    if !yes && !confirm(&format!("Migrate {} project(s)?", safe.len()))? {
        println!("Aborted.");
        return Ok(());
    }

    let mut ok = 0usize;
    let mut failed = Vec::new();
    for legacy in &safe {
        println!("\n{} {}", "→".cyan(), legacy.root.display());
        match migrate_one(&legacy.root, keep, true).await {
            Ok(()) => ok += 1,
            Err(e) => {
                println!("  {} {e}", "✗".red());
                failed.push(legacy.root.display().to_string());
            }
        }
    }

    println!("\n{}", "─── Result ──────────────────────────────────────".bold());
    println!("  migrated: {ok}/{}", safe.len());
    if !clashing.is_empty() {
        println!("  skipped (shared collection): {}", clashing.values().map(|v| v.len()).sum::<usize>());
    }
    if !failed.is_empty() {
        println!("  {} {}", "failed:".red(), failed.join(", "));
    }
    println!("  Verify with: {}", "ragpilot projects list".bold());
    Ok(())
}

// ── sync ───────────────────────────────────────────────────────────────────

/// What a registered project is missing on its own side.
struct Gap {
    root: PathBuf,
    id: String,
    /// No MCP config for the chosen agent.
    no_mcp: bool,
    /// The agent markdown has no ragpilot block, or an out-of-date one.
    no_block: bool,
    /// A brain exists, but this project has no session hooks.
    no_hooks: bool,
    /// Bytes of leading text the block already says, and the lines a tidy
    /// would lose. `None` when there is nothing redundant.
    redundant: Option<crate::agents::Redundant>,
}

impl Gap {
    fn any(&self) -> bool { self.no_mcp || self.no_block || self.no_hooks }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.no_mcp { parts.push("mcp config"); }
        if self.no_block { parts.push("agent block"); }
        if self.no_hooks { parts.push("session hooks"); }
        parts.join(", ")
    }
}

/// Bring every registered project's own files up to date with this version:
/// the MCP registration, the marked block in its agent markdown, and — when a
/// brain exists — the session hooks.
///
/// This is `init`'s project-folder half, run across the fleet. The index, the
/// registry and the collection are untouched; the only writes are into the
/// project folders, and every one of them is idempotent.
pub async fn cmd_sync(
    agent: &str,
    only: &[String],
    tidy: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    if !crate::agents::PROJECT_CLIENTS.contains(&agent) {
        anyhow::bail!(
            "'{agent}' has no per-project config. Choose one of: {}",
            crate::agents::PROJECT_CLIENTS.join(", ")
        );
    }

    let registry = Registry::load()?;
    let brain = crate::brain::exists();

    let gaps: Vec<Gap> = registry
        .projects
        .iter()
        .filter(|(path, _)| matches_only(path, only))
        .filter_map(|(path, entry)| {
            let root = PathBuf::from(path);
            root.is_dir().then(|| gap_for(&root, &entry.id, agent, brain))
        })
        .collect();

    if gaps.is_empty() && !only.is_empty() {
        anyhow::bail!(
            "No registered project matches {:?}. `ragpilot projects list` shows the paths.",
            only
        );
    }

    let pending: Vec<&Gap> = gaps.iter().filter(|g| g.any()).collect();
    let redundant: Vec<&Gap> = gaps.iter().filter(|g| g.redundant.is_some()).collect();

    println!(
        "{} {} registered project(s) · {} already current · {} to update",
        "→".cyan(),
        gaps.len(),
        gaps.len() - pending.len(),
        pending.len()
    );
    if !brain {
        println!("  {} no brain on this machine — session hooks are skipped.", "i".blue());
    }
    if !redundant.is_empty() {
        let bytes: usize = redundant.iter().filter_map(|g| g.redundant.as_ref()).map(|r| r.bytes).sum();
        println!(
            "  {} {} project(s) carry an older copy of the same doc above the block ({} KB in total).",
            if tidy { "→".cyan() } else { "i".blue() },
            redundant.len(),
            bytes / 1024
        );
        if !tidy {
            println!("     Remove it with {}", "--tidy".bold());
        }
    }

    let nothing_to_tidy = !tidy || redundant.is_empty();
    if pending.is_empty() && nothing_to_tidy {
        println!("{} Nothing to do.", "✓".green());
        return Ok(());
    }

    if !pending.is_empty() {
        println!("\n{}", "─── Missing ─────────────────────────────────────".bold());
        for gap in &pending {
            println!("  {} — {}", gap.root.display(), gap.describe().yellow());
        }
    }

    if tidy && !redundant.is_empty() {
        // Nothing is deleted before the user has seen exactly what goes. The
        // lines below are the ones the block does *not* already contain.
        let mut lost: Vec<String> = redundant
            .iter()
            .filter_map(|g| g.redundant.as_ref())
            .flat_map(|r| r.lost.clone())
            .collect();
        lost.sort();
        lost.dedup();

        println!("\n{}", "─── Lines a tidy would remove ───────────────────".bold());
        if lost.is_empty() {
            println!("  none — the block already says everything above it.");
        } else {
            for line in lost.iter().take(20) {
                println!("  {}", line.dimmed());
            }
            if lost.len() > 20 {
                println!("  … and {} more", lost.len() - 20);
            }
        }
    }

    if dry_run {
        // The hint has to be the command that does what was just described,
        // flags and all — otherwise running it does something else.
        let mut flags = String::new();
        if agent != "claude" { flags.push_str(&format!(" --agent {agent}")); }
        if !only.is_empty() { flags.push_str(&format!(" --only {}", only.join(","))); }
        if tidy { flags.push_str(" --tidy"); }
        println!("\n  Apply with: {}", format!("ragpilot projects sync{flags}").bold());
        return Ok(());
    }
    println!(
        "\n  This writes into {} project folder(s): MCP config, the marked block in the\n           agent markdown, and session hooks. Nothing else is touched.",
        pending.len()
    );
    if !yes && !confirm("Proceed?")? {
        println!("Aborted.");
        return Ok(());
    }

    let (mut done, mut failed) = (0usize, Vec::new());
    for gap in &pending {
        match sync_one(&gap.root, agent, brain) {
            Ok(()) => done += 1,
            Err(e) => {
                println!("  {} {}: {e}", "✗".red(), gap.root.display());
                failed.push(gap.id.clone());
            }
        }
    }

    let mut tidied = 0usize;
    if tidy {
        for gap in &redundant {
            let doc = gap.root.join(crate::agents::agent_doc(agent));
            match crate::agents::drop_preamble(&doc) {
                Ok(0) => {}
                Ok(bytes) => {
                    println!("  {} {} (−{} bytes)", "✓".green(), doc.display(), bytes);
                    tidied += 1;
                }
                Err(e) => println!("  {} {}: {e}", "✗".red(), doc.display()),
            }
        }
    }

    println!("\n{}", "─── Result ──────────────────────────────────────".bold());
    println!("  updated: {done}/{}", pending.len());
    if tidy {
        println!("  tidied:  {tidied}/{}", redundant.len());
    }
    if !failed.is_empty() {
        println!("  {} {}", "failed:".red(), failed.join(", "));
    }
    Ok(())
}

/// Case-insensitive substring match against the project path. An empty filter
/// matches everything — writing into every project should be what you asked
/// for, not what you get by leaving a flag off, so `--only` narrows rather than
/// being required.
fn matches_only(path: &str, only: &[String]) -> bool {
    if only.is_empty() {
        return true;
    }
    let path = path.to_lowercase();
    only.iter().any(|pattern| path.contains(&pattern.to_lowercase()))
}

fn gap_for(root: &Path, id: &str, agent: &str, brain: bool) -> Gap {
    let mcp = crate::agents::mcp_config_path(agent, root);
    let no_mcp = !mcp.map(|p| p.exists()).unwrap_or(true);

    let doc = root.join(crate::agents::agent_doc(agent));
    let no_block = std::fs::read_to_string(&doc)
        .map(|text| !text.contains(crate::agents::BLOCK_START))
        .unwrap_or(true);

    // Only Claude Code has hooks, and only when there is a brain to feed them.
    let no_hooks = brain
        && agent == "claude"
        && std::fs::read_to_string(root.join(crate::brain::hooks::CLAUDE_SETTINGS))
            .map(|text| !text.contains("brain session-start"))
            .unwrap_or(true);

    let redundant = std::fs::read_to_string(&doc)
        .ok()
        .and_then(|text| crate::agents::redundant_preamble(&text));

    Gap { root: root.to_path_buf(), id: id.to_string(), no_mcp, no_block, no_hooks, redundant }
}

/// One project. `agents::configure` already writes the MCP config, maintains
/// the marked block and installs the hooks when a brain exists — this is the
/// same code `init` runs, so the two can never drift apart.
fn sync_one(root: &Path, agent: &str, _brain: bool) -> Result<()> {
    println!("\n{} {}", "→".cyan(), root.display());
    crate::agents::configure(agent, root)
}

// ── projects ───────────────────────────────────────────────────────────────

pub async fn cmd_projects(args: &[String]) -> Result<()> {
    match args.get(2).map(String::as_str) {
        Some("list") | None => projects_list(),
        Some("rm") => {
            let id = args.get(3).cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: ragpilot projects rm <id> [--yes]")
            })?;
            let yes = args.iter().any(|a| a == "--yes" || a == "-y");
            projects_rm(&id, yes).await
        }
        Some("sync") => {
            let agent = args
                .iter()
                .position(|a| a == "--agent")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "claude".to_string());
            let only: Vec<String> = args
                .iter()
                .position(|a| a == "--only")
                .and_then(|i| args.get(i + 1))
                .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            let dry = args.iter().any(|a| a == "--dry-run");
            let yes = args.iter().any(|a| a == "--yes" || a == "-y");
            let tidy = args.iter().any(|a| a == "--tidy");
            cmd_sync(&agent, &only, tidy, dry, yes).await
        }
        Some("relink") => {
            let id = args.get(3).cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: ragpilot projects relink <id> <new-path>")
            })?;
            let path = args.get(4).cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: ragpilot projects relink <id> <new-path>")
            })?;
            projects_relink(&id, &path)
        }
        Some(other) => anyhow::bail!(
            "Unknown subcommand '{other}'. Usage: ragpilot projects [list | sync | rm <id> | relink <id> <path>]"
        ),
    }
}

fn projects_list() -> Result<()> {
    let registry = Registry::load()?;
    if registry.projects.is_empty() {
        println!("{} No registered projects yet — run `ragpilot init .` in one.", "i".blue());
        return Ok(());
    }

    println!("{}", "─── Registered projects ─────────────────────────".bold());
    for (path, entry) in &registry.projects {
        let missing = !Path::new(path).exists();
        let marker = if missing { " (path missing)".yellow() } else { "".normal() };
        println!("  {}{}", entry.id.bold(), marker);
        println!("    path:    {path}");
        println!("    data:    {}", paths::project_dir(&entry.id).display());
        println!(
            "    indexed: {}",
            entry.last_indexed.as_deref().unwrap_or("never")
        );
    }
    if registry.projects.values().count() > 0 {
        println!(
            "\n  A missing path usually means the folder moved: {}",
            "ragpilot projects relink <id> <new-path>".dimmed()
        );
    }
    Ok(())
}

async fn projects_rm(id: &str, yes: bool) -> Result<()> {
    let mut registry = Registry::load()?;
    let (path, _) = registry
        .lookup_by_id(id)
        .ok_or_else(|| anyhow::anyhow!("No project registered with id '{id}'"))?;
    let path = path.to_string();
    let data_dir = paths::project_dir(id);

    println!("{} This removes:", "!".yellow());
    println!("    registry entry:  {path}");
    println!("    data directory:  {}", data_dir.display());
    println!("    Qdrant collection for '{id}'");
    println!("  The project's own files are NOT touched.");
    if !yes && !confirm("Proceed?")? {
        println!("Aborted.");
        return Ok(());
    }

    // Qdrant first: if it fails, the registry entry still points at the data.
    match delete_collection_for(id, &data_dir).await {
        Ok(Some(name)) => println!("{} Deleted collection '{name}'", "✓".green()),
        Ok(None) => println!("{} No collection found — nothing to delete", "i".blue()),
        Err(e) => println!("{} Collection not deleted: {e}", "!".yellow()),
    }

    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)
            .with_context(|| format!("Cannot remove {}", data_dir.display()))?;
        println!("{} Removed {}", "✓".green(), data_dir.display());
    }

    registry.remove_id(id);
    registry.save()?;
    println!("{} Unregistered '{id}'", "✓".green());
    Ok(())
}

/// Delete the collection a project uses, following an alias to the physical
/// collection so migrated projects do not leave their vectors behind.
async fn delete_collection_for(id: &str, data_dir: &Path) -> Result<Option<String>> {
    let config_path = data_dir.join("config.toml");
    if !config_path.exists() {
        return Ok(None);
    }
    let config = Config::load(&config_path)?;
    let name = config
        .qdrant
        .collection
        .clone()
        .map(|c| paths::normalize_collection(&c))
        .unwrap_or_else(|| id.to_string());

    let store = crate::store::qdrant::QdrantStore::new(&config.qdrant)?;
    if let Some(physical) = store.alias_target(&name).await {
        store.drop_alias(&name).await.ok();
        store.delete_named(&physical).await?;
        return Ok(Some(physical));
    }
    if !store.exists(&name).await {
        return Ok(None);
    }
    store.delete_named(&name).await?;
    Ok(Some(name))
}

fn projects_relink(id: &str, new_path: &str) -> Result<()> {
    let target = paths::canonical(Path::new(new_path))
        .with_context(|| format!("Cannot resolve {new_path}"))?;

    let mut registry = Registry::load()?;
    registry.relink(id, &target)?;
    registry.save()?;

    println!("{} '{}' now points at {}", "✓".green(), id.bold(), target.display());
    println!(
        "  The data directory is unchanged: {}",
        paths::project_dir(id).display()
    );
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Move a file or directory, falling back to copy+delete when the data root is
/// on a different filesystem than the project (a common setup).
fn move_path(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        std::fs::remove_dir_all(from)?;
    } else {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
    }
    Ok(())
}

/// Copy a file or directory, leaving the source in place.
fn copy_path(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        copy_dir(from, to)
    } else {
        std::fs::copy(from, to)?;
        Ok(())
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    use std::io::Write;
    print!("{} {question} [y/N] ", "?".yellow());
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-migrate-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn legacy_at(root: &Path, name: &str, collection: &str) {
        let rag = root.join(".rag");
        std::fs::create_dir_all(&rag).unwrap();
        std::fs::write(
            rag.join("config.toml"),
            format!(
                "[project]\nname = \"{name}\"\n\n[embedding]\nprovider = \"local\"\n\n\
                 [qdrant]\nurl = \"http://localhost:6334\"\ncollection = \"{collection}\"\n\n\
                 [indexing]\nchunk_size = 700\nchunk_overlap = 80\ninclude_extensions = [\"rs\"]\n\
                 include_dirs = []\n\n[mcp]\ncontext_chunks = 4\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn scan_finds_nested_projects_and_skips_build_directories() {
        let dir = scratch("scan");
        let tree = dir.join("tree");
        for rel in ["app", "app/packages/api", "node_modules/dep", "target/debug/copy"] {
            std::fs::create_dir_all(tree.join(rel)).unwrap();
            legacy_at(&tree.join(rel), &rel.replace('/', "-"), "coll");
        }

        let registry = Registry::load_from(&dir.join("registry.json")).unwrap();
        let found = scan(&tree, &registry);

        let names: Vec<String> = found
            .iter()
            .map(|l| l.root.strip_prefix(&tree).unwrap().to_string_lossy().to_string())
            .collect();
        // A monorepo can index a subdirectory separately, so nesting is kept…
        assert!(names.contains(&"app".to_string()));
        assert!(names.contains(&"app/packages/api".to_string()));
        // …but build output and vendored code are never projects.
        assert!(!names.iter().any(|n| n.contains("node_modules")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("target")), "{names:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_reports_the_id_and_marks_already_migrated_projects() {
        let dir = scratch("registered");
        let tree = dir.join("tree");
        let (a, b) = (tree.join("alpha"), tree.join("beta"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        legacy_at(&a, "alpha", "alpha");
        legacy_at(&b, "beta", "beta");

        let mut registry = Registry::load_from(&dir.join("registry.json")).unwrap();
        registry.upsert(&paths::canonical(&a).unwrap());

        let found = scan(&tree, &registry);
        assert_eq!(found.len(), 2);
        let alpha = found.iter().find(|l| l.project_name == "alpha").unwrap();
        let beta = found.iter().find(|l| l.project_name == "beta").unwrap();

        assert!(alpha.registered, "a registered project should be reported as migrated");
        assert!(!beta.registered);
        assert!(beta.id.starts_with("beta-"), "{}", beta.id);
        assert_eq!(beta.collection, "beta");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collisions_catch_two_projects_sharing_one_legacy_collection() {
        let mk = |root: &str, collection: &str| Legacy {
            root: PathBuf::from(root),
            project_name: "x".into(),
            collection: collection.into(),
            id: "x-1".into(),
            registered: false,
            size_kb: 0,
        };
        let found = vec![
            mk("/dev/api", "api"),
            mk("/tmp/api", "api"),
            mk("/dev/web", "web"),
        ];

        let clashing = collisions(&found);
        assert_eq!(clashing.len(), 1, "only the shared name is a collision");
        assert_eq!(clashing["api"].len(), 2);
        assert!(!clashing.contains_key("web"));

        // Nothing shared → nothing flagged.
        assert!(collisions(&[mk("/dev/a", "a"), mk("/dev/b", "b")]).is_empty());
    }

    #[test]
    fn unpin_collection_keeps_everything_else() {
        let dir = scratch("unpin");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"demo\"\n\n[qdrant]\nurl = \"http://localhost:6334\"\ncollection = \"demo\"\n# keep me\n",
        )
        .unwrap();

        unpin_collection(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("collection ="));
        assert!(text.contains("url = \"http://localhost:6334\""));
        assert!(text.contains("# keep me"));
        assert!(text.contains("name = \"demo\""));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn move_path_handles_files_and_directories() {
        let dir = scratch("move");
        let src_dir = dir.join("src");
        std::fs::create_dir_all(src_dir.join("nested")).unwrap();
        std::fs::write(src_dir.join("nested").join("a.txt"), "hello").unwrap();
        std::fs::write(dir.join("file.txt"), "world").unwrap();

        move_path(&dir.join("file.txt"), &dir.join("moved.txt")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("moved.txt")).unwrap(), "world");
        assert!(!dir.join("file.txt").exists());

        move_path(&src_dir, &dir.join("dst")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("dst").join("nested").join("a.txt")).unwrap(),
            "hello"
        );
        assert!(!src_dir.exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copy_dir_recreates_the_whole_tree() {
        let dir = scratch("copy");
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("deep.txt"), "x").unwrap();
        std::fs::write(dir.join("a").join("top.txt"), "y").unwrap();

        copy_dir(&dir.join("a"), &dir.join("copy")).unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("copy").join("top.txt")).unwrap(), "y");
        assert_eq!(
            std::fs::read_to_string(dir.join("copy").join("b").join("deep.txt")).unwrap(),
            "x"
        );
        // The source survives a copy.
        assert!(dir.join("a").join("top.txt").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-sync-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bare_project_is_missing_everything() {
        let dir = scratch("bare");
        let gap = gap_for(&dir, "bare-1", "claude", true);

        assert!(gap.no_mcp && gap.no_block && gap.no_hooks);
        assert!(gap.any());
        for part in ["mcp config", "agent block", "session hooks"] {
            assert!(gap.describe().contains(part), "{}", gap.describe());
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_configured_project_reports_no_gap() {
        let dir = scratch("configured");
        std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
        std::fs::write(
            dir.join("CLAUDE.md"),
            format!("# rules\n\n{}\nbody\n{}\n", crate::agents::BLOCK_START, crate::agents::BLOCK_END),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(crate::brain::hooks::CLAUDE_SETTINGS),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"ragpilot brain session-start"}]}]}}"#,
        )
        .unwrap();

        let gap = gap_for(&dir, "configured-1", "claude", true);
        assert!(!gap.any(), "{}", gap.describe());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hooks_are_only_expected_where_they_can_work() {
        let dir = scratch("hooks");

        // No brain on this machine: hooks are not a gap.
        assert!(!gap_for(&dir, "x", "claude", false).no_hooks);
        // Another client has no hook mechanism at all.
        assert!(!gap_for(&dir, "x", "codex", true).no_hooks);
        // Claude Code with a brain: a gap.
        assert!(gap_for(&dir, "x", "claude", true).no_hooks);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_project_client_has_a_config_path_and_a_doc() {
        for agent in crate::agents::PROJECT_CLIENTS {
            assert!(
                crate::agents::mcp_config_path(agent, Path::new("/p")).is_some(),
                "{agent} has no per-project config path"
            );
            assert!(crate::agents::agent_doc(agent).ends_with(".md"));
        }
        // A global-only client has neither.
        assert!(crate::agents::mcp_config_path("windsurf", Path::new("/p")).is_none());
    }
}

#[cfg(test)]
mod only_tests {
    use super::*;

    #[test]
    fn an_empty_filter_matches_everything() {
        // Leaving the flag off must not silently narrow the run.
        assert!(matches_only("/home/a/Projects/anything", &[]));
    }

    #[test]
    fn only_matches_a_case_insensitive_substring_of_the_path() {
        let f = vec!["bitigdb".to_string()];
        assert!(matches_only("/home/a/Projects/Desktop/BitigDB", &f));
        assert!(!matches_only("/home/a/Projects/Desktop/PosPC", &f));

        // Several patterns are a union, not an intersection.
        let f = vec!["bitigdb".to_string(), "pospc".to_string()];
        assert!(matches_only("/home/a/Projects/Desktop/BitigDB", &f));
        assert!(matches_only("/home/a/Projects/Desktop/PosPC", &f));
        assert!(!matches_only("/home/a/Projects/Web/Orilyon", &f));

        // A directory segment works as well as a project name.
        assert!(matches_only("/home/a/Projects/Web/Orilyon", &["/web/".to_string()]));
    }
}

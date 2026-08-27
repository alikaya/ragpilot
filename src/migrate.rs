//! `ragpilot migrate` and `ragpilot projects` (Phase 4).
//!
//! Everything that makes the move from a project-local `.rag/` to the global
//! data root safe and reversible-by-inspection: the data files are moved, the
//! project is registered, and the existing Qdrant index is *aliased* to its new
//! name rather than rebuilt — migrating is a rename, not a re-embed.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::paths::{self, ProjectPaths, Registry};

/// Files and directories that make up a project's data.
const MOVABLE: &[&str] = &["config.toml", "state.json", "stores.db", "queries"];

// ── migrate ────────────────────────────────────────────────────────────────

pub async fn cmd_migrate(keep: bool, yes: bool) -> Result<()> {
    let root = paths::canonical(&std::env::current_dir()?)?;
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
            "Unknown subcommand '{other}'. Usage: ragpilot projects [list | rm <id> | relink <id> <path>]"
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
    use std::path::PathBuf;
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

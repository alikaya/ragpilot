//! The brain — a second-brain vault that lives beside the project indexes but
//! belongs to no project (Phase A).
//!
//! ```text
//! <data_root>/brain/          git repo
//!   config.toml               engine, model, schedule, budgets
//!   persona.md                who the agent is, written at setup
//!   daily/YYYY-MM-DD.md       raw session flushes
//!   knowledge/*.md            compiler output
//!   skills/*.md               learned procedures
//!   inbox/                    dropped in by the user, digested by the compiler
//!   archive/takeout/          imported chat history
//! ```
//!
//! Markdown is the source of truth; the Qdrant collection is a retrieval layer
//! rebuilt from it. That ordering is what makes the vault greppable, diffable
//! and recoverable — and it is why nothing here ever deletes a note.

// The vault surface is complete on purpose: the `daily`/`knowledge`/`skills`
// path helpers and `CompilerEngine::complete` are what Phase B's MCP tools and
// Phase D's compiler call. All of it is exercised by the tests in this tree;
// the allow only silences "not called from `src/` yet".
#![allow(dead_code)]

pub mod compile;
pub mod config;
pub mod doctor;
pub mod engine;
pub mod hooks;
pub mod import;
pub mod runtime;
pub mod schedule;
pub mod session;
pub mod vault;

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::paths;
use config::{BrainConfig, SCHEMA_VERSION};

/// Directories created by `brain init`, in the order they are reported.
const TREE: &[&str] = &["daily", "knowledge", "skills", "inbox", "archive/takeout"];

/// Files that are derived from the markdown and must never be committed.
const GITIGNORE: &str = "\
# Derived from the markdown — rebuilt by `ragpilot brain index`.
state.json
stores.db
index.lock
.sessions.json
.compile.json
.missed-flush
.staging/
*.tmp
";

// ── Layout ─────────────────────────────────────────────────────────────────

pub fn dir() -> PathBuf { paths::brain_dir() }
pub fn config_path() -> PathBuf { dir().join("config.toml") }
pub fn persona_path() -> PathBuf { dir().join("persona.md") }
pub fn daily_dir() -> PathBuf { dir().join("daily") }
pub fn knowledge_dir() -> PathBuf { dir().join("knowledge") }
pub fn skills_dir() -> PathBuf { dir().join("skills") }
pub fn inbox_dir() -> PathBuf { dir().join("inbox") }
pub fn takeout_dir() -> PathBuf { dir().join("archive").join("takeout") }

/// Whether a brain has been set up on this machine.
pub fn exists() -> bool { config_path().exists() }

// ── init ───────────────────────────────────────────────────────────────────

pub async fn cmd_init(engine_override: Option<&str>) -> Result<()> {
    // Reject a typo before touching anything — silently falling back to the
    // default would leave the user believing they picked an engine.
    if let Some(name) = engine_override {
        if !engine::ENGINE_NAMES.contains(&name) {
            anyhow::bail!(
                "Unknown compiler engine '{name}'. Known engines: {}",
                engine::ENGINE_NAMES.join(", ")
            );
        }
    }

    let root = dir();
    let upgrading = exists();

    if upgrading {
        println!("{} Existing brain at {}", "i".blue(), root.display());
    } else {
        println!("{} Creating brain at {}", "→".cyan(), root.display());
    }

    // 1. Directory tree. Creating a directory that is already there is a
    //    no-op, so an upgrade fills in anything a previous version lacked.
    std::fs::create_dir_all(&root)
        .with_context(|| format!("Cannot create {}", root.display()))?;
    for rel in TREE {
        std::fs::create_dir_all(root.join(rel))?;
    }
    println!("{} Tree: {}", "✓".green(), TREE.join(", "));

    // 2. Git — the vault's undo button. Every compile commits, so a bad
    //    compilation is one `git revert` away.
    init_git(&root)?;

    // 3. Persona. Written once; never overwritten, because by then it is the
    //    user's own text.
    if persona_path().exists() {
        println!("{} persona.md (kept)", "i".blue());
    } else {
        let persona = ask_persona()?;
        std::fs::write(persona_path(), persona)?;
        println!("{} persona.md", "✓".green());
    }

    // 4. Config.
    let engine_name = engine_override.unwrap_or("claude-cli");
    if config_path().exists() {
        upgrade_schema()?;
    } else {
        let model = if engine_name == "gemini-api" { "gemini-2.5-flash" } else { "haiku" };
        std::fs::write(config_path(), BrainConfig::template(engine_name, model))?;
        println!("{} config.toml (engine: {})", "✓".green(), engine_name.bold());
    }

    // 5. Engine health — say it now, not at 18:00 when the scheduled compile
    //    quietly does nothing.
    let cfg = BrainConfig::load(&config_path())?;
    report_engine(&cfg, engine_override);

    // 5b. Rules and threads. Created empty and never overwritten — by the
    //     second run they hold the user's own corrections and open work.
    for (path, body) in [
        (vault::rules_path(), vault::rules_template(&now_date())),
        (vault::threads_path(), vault::threads_template(&now_date())),
    ] {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if path.exists() {
            println!("{} {name} (kept)", "i".blue());
        } else {
            std::fs::write(&path, body)?;
            println!("{} {name}", "✓".green());
        }
    }

    // An upgraded vault keeps its open work in the last session block. Carry it
    // into the standing list, or the loader — which prefers that list once it
    // holds anything — would quietly drop months of it.
    if vault::active_threads().is_empty() {
        let carried = vault::threads_from_dailies();
        if !carried.is_empty() {
            vault::update_threads(&carried, &[])?;
            println!("{} threads.md seeded with {} open thread(s) from the log", "✓".green(), carried.len());
        }
    }

    // 6. First commit, so "revert the compile" has something to revert to.
    //    A vault whose history starts empty cannot be rolled back at all.
    commit_initial(&root);

    // 7. Index whatever is already there (a fresh brain: persona + nothing).
    let indexed = index().await?;
    println!("{} Indexed {indexed} file(s) into '{}'", "✓".green(), paths::BRAIN_COLLECTION);

    println!(
        "\n{} Brain {}.",
        "✓".green(),
        if upgrading { "upgraded" } else { "ready" }
    );
    Ok(())
}

/// `git init` unless the vault is already a repo. A brain without git still
/// works — it just loses the ability to undo a compile.
fn init_git(root: &Path) -> Result<()> {
    if root.join(".git").exists() {
        println!("{} git repo (already)", "i".blue());
    } else {
        match std::process::Command::new("git").arg("init").arg("-q").current_dir(root).status() {
            Ok(s) if s.success() => println!("{} git init", "✓".green()),
            _ => println!(
                "{} git not available — the vault works, but compiles cannot be reverted.",
                "!".yellow()
            ),
        }
    }
    let gitignore = root.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, GITIGNORE)?;
        println!("{} .gitignore", "✓".green());
    }
    Ok(())
}

/// Make the first commit when the repo has none. Best-effort and quiet: a
/// machine without git still gets a working vault.
fn commit_initial(root: &Path) {
    let has_commits = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true); // no git → nothing to do
    if has_commits {
        return;
    }

    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .status();
    let committed = add.map(|s| s.success()).unwrap_or(false)
        && std::process::Command::new("git")
            .args(["commit", "-q", "-m", "brain: initial vault"])
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if committed {
        println!("{} git commit (initial vault)", "✓".green());
    }
}

/// Bring an older brain forward. **Never destructive**: the schema version is
/// rewritten, the user's settings are left exactly as they are.
fn upgrade_schema() -> Result<()> {
    let path = config_path();
    let cfg = BrainConfig::load(&path)?;
    let found = cfg.brain.schema_version;

    if found == SCHEMA_VERSION {
        println!("{} config.toml (schema v{found}, current)", "i".blue());
        return Ok(());
    }
    if found > SCHEMA_VERSION {
        anyhow::bail!(
            "This brain is schema v{found}, but this ragpilot only understands v{SCHEMA_VERSION}. \
             Upgrade ragpilot rather than downgrading the brain."
        );
    }

    let text = std::fs::read_to_string(&path)?;
    let updated: Vec<String> = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("schema_version") {
                format!("schema_version = {SCHEMA_VERSION}")
            } else {
                line.to_string()
            }
        })
        .collect();
    std::fs::write(&path, updated.join("\n") + "\n")?;
    println!("{} config.toml upgraded: schema v{found} → v{SCHEMA_VERSION}", "✓".green());
    Ok(())
}

fn report_engine(cfg: &BrainConfig, engine_override: Option<&str>) {
    match engine::create(cfg, engine_override) {
        Ok(e) => match e.available() {
            Ok(()) => println!("{} Compiler engine '{}' is ready", "✓".green(), e.name()),
            Err(why) => {
                println!("{} Compiler engine '{}' is not usable yet:", "!".yellow(), e.name());
                println!("    {why}");
            }
        },
        Err(why) => println!("{} {why}", "!".yellow()),
    }
}

// ── persona ────────────────────────────────────────────────────────────────

fn ask_persona() -> Result<String> {
    println!("\n{}", "─── Who is your agent? ──────────────────────────".bold());
    let name = ask("Name", "Pilot")?;
    let character = ask("Character / tone", "direct, concise, no flattery")?;
    let areas = ask("What you work on (comma separated)", "software")?;

    Ok(format!(
        "---\nname: {name}\nupdated: {}\n---\n\n\
         # {name}\n\n\
         ## Character\n\n{character}\n\n\
         ## Areas\n\n{}\n\n\
         ## Notes\n\n\
         This file is yours — edit it freely. The compiler reads it and never rewrites it.\n",
        now_date(),
        areas
            .split(',')
            .map(|a| format!("- {}", a.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

/// Prompt with a default. A non-interactive run (no tty, piped stdin) takes the
/// default instead of hanging or writing an empty persona.
fn ask(question: &str, default: &str) -> Result<String> {
    print!("  {question} [{}]: ", default.dimmed());
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        println!();
        return Ok(default.to_string());
    }
    let answer = line.trim();
    Ok(if answer.is_empty() { default.to_string() } else { answer.to_string() })
}

fn now_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ── indexing ───────────────────────────────────────────────────────────────

/// The indexing config for the vault. It is a markdown tree, not a codebase:
/// only `.md` is indexed, and the derived files are excluded.
pub(crate) fn index_config() -> Result<crate::config::Config> {
    crate::config::Config::from_toml_str(&format!(
        r#"
[project]
name = "{}"

[embedding]
provider = "local"

[qdrant]
url = "http://localhost:6334"

[indexing]
chunk_size = 700
chunk_overlap = 80
include_extensions = ["md"]
exclude_dirs = [".git", "archive", ".staging"]
include_dirs = []

[mcp]
context_chunks = 4
"#,
        paths::BRAIN_COLLECTION
    ))
}

/// (Re)index the vault into `ragpilot_brain`. Returns how many files were
/// indexed. Safe to call repeatedly — it is incremental.
pub async fn index() -> Result<usize> {
    let config = index_config()?;
    let brain_paths = paths::ProjectPaths::brain();
    let orch = crate::indexer::build_orchestrator_at(&brain_paths, &config)?;
    let result = orch.ensure_index(false).await?;
    Ok(result.indexed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_covers_every_derived_file() {
        for derived in ["state.json", "stores.db", "index.lock"] {
            assert!(GITIGNORE.contains(derived), "{derived} would be committed");
        }
    }

    #[test]
    fn tree_matches_the_documented_layout() {
        assert_eq!(TREE, ["daily", "knowledge", "skills", "inbox", "archive/takeout"]);
    }

    #[test]
    fn brain_paths_all_live_under_one_root() {
        let root = dir();
        for p in [config_path(), persona_path(), daily_dir(), knowledge_dir(), skills_dir(), inbox_dir(), takeout_dir()] {
            assert!(p.starts_with(&root), "{} escaped the vault", p.display());
        }
    }

    #[test]
    fn the_vault_indexes_only_markdown() {
        let cfg = index_config().unwrap();
        assert_eq!(cfg.indexing.include_extensions, vec!["md".to_string()]);
        assert!(cfg.indexing.exclude_dirs.contains(&".git".to_string()));
        assert_eq!(cfg.project.name, paths::BRAIN_COLLECTION);
    }

    #[test]
    fn the_vault_uses_the_fixed_collection() {
        let paths = paths::ProjectPaths::brain();
        assert_eq!(paths.collection(None, "anything"), paths::BRAIN_COLLECTION);
        assert_eq!(paths.state(), dir().join("state.json"));
        assert_eq!(paths.stores_db(), dir().join("stores.db"));
    }
}

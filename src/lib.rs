//! RagPilot core — token-efficient, local-first code intelligence over MCP.
//!
//! This crate is both the `ragpilot` binary and a library. The library surface
//! is deliberately small: the full CLI ([`run`] / [`run_with_observer`]) and a
//! generic observation seam ([`ToolObserver`]) that lets a *separate* build
//! attach behaviour after each MCP exchange without the core knowing anything
//! about it. The open-source core ships no observer; a closed distribution can
//! supply one. Nothing here reaches out to a network on its own.

mod agents;
mod brain;
mod config;
mod dashboard;
mod embedder;
mod indexer;
mod migrate;
mod orchestrator;
mod parser;
mod paths;
mod semantic_diff;
mod skeleton;
mod store;
mod tokens;
mod watcher;
mod wizard;

pub mod mcp;

use std::sync::Arc;

pub use mcp::protocol::{McpRequest, McpResponse};
pub use mcp::{run_server, run_server_with, ObserverContext, ToolObserver};

/// Run the `ragpilot` CLI with no observer — the open-source binary's entry.
pub fn run() -> anyhow::Result<()> {
    run_with_observer(None)
}

/// Run the `ragpilot` CLI, attaching `observer` to the MCP server (the
/// `--mcp-server` path). A separate, closed distribution passes `Some(_)` to
/// add usage/audit reporting; the open-source binary passes `None`.
pub fn run_with_observer(observer: Option<Arc<dyn ToolObserver>>) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(dispatch(observer))
}

async fn dispatch(observer: Option<Arc<dyn ToolObserver>>) -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("ragpilot {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }

        Some("--mcp-server") => mcp::run_server_with(observer).await,

        Some("init") => {
            // "ragpilot init <folder> <agent>"  →  setup mode
            // "ragpilot init [--force]"         →  index only
            let has_folder = args.get(2).map(|a| !a.starts_with('-')).unwrap_or(false);
            let has_agent = args.get(3).map(|a| !a.starts_with('-')).unwrap_or(false);
            if has_folder && has_agent {
                cmd_setup(&args).await
            } else {
                let force = args.iter().any(|a| a == "--force");
                indexer::cmd_init(force).await
            }
        }

        Some("update") => indexer::cmd_update().await,
        Some("status") => indexer::cmd_status().await,
        Some("stats") => indexer::cmd_stats().await,
        Some("skeleton") => cmd_skeleton(&args).await,
        Some("review") => cmd_review(&args).await,

        Some("clean") => {
            let yes = args.iter().any(|a| a == "--yes" || a == "-y");
            indexer::cmd_clean(yes).await
        }

        Some("setup") => cmd_setup(&args).await,

        Some("migrate") => {
            let keep = args.iter().any(|a| a == "--keep");
            let yes = args.iter().any(|a| a == "--yes" || a == "-y");
            // `--scan <dir>` takes an inventory; `--all <dir>` migrates it.
            let scan = flag_value(&args, "--scan");
            let all = flag_value(&args, "--all");
            match (scan, all) {
                (_, Some(dir)) => migrate::cmd_scan(std::path::Path::new(&dir), true, keep, yes).await,
                (Some(dir), None) => migrate::cmd_scan(std::path::Path::new(&dir), false, keep, yes).await,
                (None, None) => migrate::cmd_migrate(keep, yes).await,
            }
        }
        Some("projects") => migrate::cmd_projects(&args).await,

        Some("brain") => {
            let engine = flag_value(&args, "--engine");
            match args.get(2).map(String::as_str) {
                Some("init") => brain::cmd_init(engine.as_deref()).await,
                Some("index") => {
                    let n = brain::index().await?;
                    println!("Indexed {n} file(s) into '{}'.", paths::BRAIN_COLLECTION);
                    Ok(())
                }
                Some("session-start") => {
                    let max = flag_value(&args, "--max-tokens").and_then(|v| v.parse().ok());
                    brain::session::cmd_session_start(max)
                }
                Some("session-end") => {
                    let transcript = flag_value(&args, "--transcript").map(std::path::PathBuf::from);
                    brain::session::cmd_session_end(transcript, engine.as_deref()).await
                }
                Some("hooks") => {
                    let root = std::env::current_dir()?;
                    brain::hooks::install_claude(&root)
                }
                Some("compile") => {
                    let light = args.iter().any(|a| a == "--light");
                    brain::compile::cmd_compile(light, engine.as_deref()).await
                }
                Some("schedule") => brain::schedule::cmd_schedule(&args),
                Some("import") => {
                    let target = args
                        .get(3)
                        .filter(|a| !a.starts_with('-'))
                        .ok_or_else(|| anyhow::anyhow!(
                            "Usage: ragpilot brain import <file|dir> [--limit N] [--since YYYY-MM-DD]"
                        ))?;
                    let opts = brain::import::ImportOptions {
                        limit: flag_value(&args, "--limit").and_then(|v| v.parse().ok()),
                        since: flag_value(&args, "--since"),
                        engine: engine.clone(),
                    };
                    brain::import::cmd_import(std::path::Path::new(target), opts).await
                }
                Some("doctor") => {
                    let fix = args.iter().any(|a| a == "--fix");
                    brain::doctor::cmd_doctor(fix).await
                }
                other => anyhow::bail!(
                    "Unknown brain subcommand {:?}. Usage: ragpilot brain [init | index | compile | import | doctor | schedule | hooks | session-start | session-end] [--engine <name>] [--light] [--fix]\n  Engines: {}",
                    other.unwrap_or("(none)"),
                    brain::engine::ENGINE_NAMES.join(", ")
                ),
            }
        }

        Some("paths") => cmd_paths(),

        Some("dashboard") => {
            let port = flag_value(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(7777);
            let open = args.iter().any(|a| a == "--open");
            dashboard::cmd_dashboard(port, open).await
        }

        Some("hooks") => cmd_hooks().await,
        Some("doctor") => cmd_doctor().await,

        _ => {
            eprintln!(
                "ragpilot — RAG MCP Server for Claude Code\n\
                 \n\
                 Usage:\n\
                   ragpilot --mcp-server              Start MCP server (stdio)\n\
                   ragpilot --mcp-server --root <dir>  Start MCP server pinned to <dir> (for global clients)\n\
                   ragpilot init <folder> <agent>     Init project + agent config\n\
                                                     agents: claude codex cursor vscode opencode windsurf antigravity all\n\
                   ragpilot init [--force]            Index current project\n\
                   ragpilot setup <folder> <agent>    Alias for 'ragpilot init <folder> <agent>'\n\
                   ragpilot migrate [--keep] [-y]  Move this project's .rag/ into the global data dir\n\
                   ragpilot migrate --scan <dir>   Find every legacy .rag/ project under <dir>\n\
                   ragpilot migrate --all <dir>    Migrate all of them\n\
                   ragpilot projects list          List registered projects\n\
                   ragpilot projects rm <id>       Unregister + delete its data and collection\n\
                   ragpilot projects sync [--agent <a>] [--dry-run]  Bring every project's files up to date\n\
                   ragpilot projects relink <id> <path>  Point a project at its new folder\n\
                   ragpilot brain init [--engine <name>]  Set up the second-brain vault\n\
                   ragpilot brain index            Re-index the brain vault\n\
                   ragpilot brain compile [--light]  Distil daily logs into knowledge notes\n\
                   ragpilot brain import <path>    Import a chat archive (ChatGPT/Claude/Codex/md)\n\
                   ragpilot brain doctor [--fix]   Check the vault and repair what is safe\n\
                   ragpilot brain schedule [--install|--remove|--print]  Daily compile\n\
                   ragpilot brain hooks            Install the Claude Code session hooks here\n\
                   ragpilot update                 Re-index changed files\n\
                   ragpilot status                 Show index statistics\n\
                   ragpilot paths                  Print where this project's data lives\n\
                   ragpilot dashboard [--open]     Local dashboard: projects + brain\n\
\n\
                   ragpilot stats                  Show last context_bundle token savings\n\
                   ragpilot skeleton <file>        Print a token-efficient skeleton of a file\n\
                   ragpilot review [<ref>]         Semantic diff: changed symbols + blast radius\n\
\n\
                   ragpilot clean [--yes]          Delete Qdrant collection\n\
                   ragpilot hooks                  Install git post-commit/post-merge hooks\n\
                   ragpilot doctor                 Check installation and configuration\n\
                   ragpilot --version              Print version\n\
                 \n\
                 Examples:\n\
                   ragpilot init /path/to/myapp codex\n\
                   ragpilot init /path/to/myapp claude\n\
                   ragpilot init . claude\n\
                 \n\
                 MCP registration (.mcp.json):\n\
                   {{\"mcpServers\":{{\"ragpilot\":{{\"type\":\"stdio\",\"command\":\"ragpilot\",\"args\":[\"--mcp-server\"]}}}}}}"
            );
            std::process::exit(1);
        }
    }
}

// ─── ragpilot review ───────────────────────────────────────────────────────────

async fn cmd_review(args: &[String]) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let target = args.get(2).map(|s| s.as_str());
    let report = semantic_diff::analyze(&root, target).await?;
    print!("{}", semantic_diff::render(&report));
    Ok(())
}

// ─── ragpilot skeleton ─────────────────────────────────────────────────────────

async fn cmd_skeleton(args: &[String]) -> anyhow::Result<()> {
    use colored::Colorize;

    let path = args
        .get(2)
        .ok_or_else(|| anyhow::anyhow!("Usage: ragpilot skeleton <file>"))?;
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Cannot read '{path}': {e}"))?;
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let language = indexer::file_language(ext);

    let sk = skeleton::skeletonize(&content, language);
    let full = tokens::estimate(&content);
    let skel = tokens::estimate(&sk);
    let ratio = if skel == 0 { 0.0 } else { full as f64 / skel as f64 };

    // Skeleton to stdout (pipeable); the summary to stderr.
    print!("{sk}");
    eprintln!(
        "{}",
        format!(
            "── {language} | full {full} tok → skeleton {skel} tok ({ratio:.2}x reduction)"
        )
        .dimmed()
    );
    Ok(())
}

// ─── rag hooks ───────────────────────────────────────────────────────────────

async fn cmd_hooks() -> anyhow::Result<()> {
    use colored::Colorize;
    use std::io::Write as IoWrite;

    let root = std::env::current_dir()?;
    let hooks_dir = root.join(".git").join("hooks");

    if !hooks_dir.exists() {
        anyhow::bail!(
            "No .git/hooks directory found. Are you in a git repository?\n\
             Run 'git init' first."
        );
    }

    const HOOK_CONTENT: &str = "#!/bin/sh\nragpilot update 2>/dev/null || true\n";

    for hook_name in &["post-commit", "post-merge"] {
        let hook_path = hooks_dir.join(hook_name);

        // Don't overwrite existing hooks — append if needed
        if hook_path.exists() {
            let existing = std::fs::read_to_string(&hook_path)?;
            if existing.contains("ragpilot update") {
                println!("{} {} (already contains ragpilot update)", "✓".green(), hook_name);
                continue;
            }
            // Append to existing hook
            let mut file = std::fs::OpenOptions::new().append(true).open(&hook_path)?;
            writeln!(file, "\n# ragpilot: auto-reindex on commit\nragpilot update 2>/dev/null || true")?;
            println!("{} {} (appended)", "✓".green(), hook_name);
        } else {
            std::fs::write(&hook_path, HOOK_CONTENT)?;
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&hook_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&hook_path, perms)?;
            }
            println!("{} {} created", "✓".green(), hook_name);
        }
    }

    println!("{} Git hooks installed. Index will auto-update on commit/merge.", "✓".green());
    Ok(())
}

// ─── rag doctor ──────────────────────────────────────────────────────────────

async fn cmd_doctor() -> anyhow::Result<()> {
    use colored::Colorize;

    let root = std::env::current_dir()?;
    let project_paths = paths::ProjectPaths::resolve(&root);
    paths::nudge_if_legacy(&project_paths);
    let config_path = project_paths.config();
    let state_path = project_paths.state();
    let stores_path = project_paths.stores_db();

    println!("{}", "─── ragpilot doctor ────────────────────────────".bold());

    // 1. Config
    check("Config file exists", config_path.exists());
    check("State file exists", state_path.exists());
    check("SQLite stores exist", stores_path.exists());

    // 2. Qdrant connectivity
    if config_path.exists() {
        if let Ok(cfg) = config::Config::load(&config_path) {
            let qdrant_ok = tokio::task::spawn_blocking({
                let url = cfg.qdrant.url.clone();
                move || {
                    let client = qdrant_client::Qdrant::from_url(&url).build();
                    client.is_ok()
                }
            })
            .await
            .unwrap_or(false);
            check(&format!("Qdrant reachable ({})", cfg.qdrant.url), qdrant_ok);

            // Offline readiness: with the local provider, the embedding model
            // must already be in the cache for air-gapped operation.
            if cfg.embedding.provider == "local" {
                let cache = embedder::local::resolve_cache_dir(&cfg.embedding.local, &root);
                let cached = embedder::local::cache_has_model(&cache);
                check(&format!("Embedding model cached ({})", cache.display()), cached);
                if !cached {
                    println!(
                        "     First run needs internet to download the model (~130MB).\n     \
                         For offline/air-gapped machines, copy a populated cache to that path."
                    );
                }
            }

            println!("\n{}", "─── Resource Warnings ───────────────────────────".bold());
            if cfg.indexing.include_dirs.is_empty() {
                println!("  ! The whole project will be indexed; resource usage may grow on large projects.");
            }
            if cfg.indexing.include_extensions.len() > 8 {
                println!("  ! Indexing a large number of file types.");
            }
            if cfg.indexing.max_file_size_kb > 500 {
                println!("  ! Large files may increase RAM/CPU usage.");
            }
            if cfg.mcp.context_chunks > 6 {
                println!("  ! MCP context results may be larger than necessary.");
            }
            if cfg.watcher.enabled {
                println!("  ! The watcher may trigger continuous re-indexing on large projects.");
            }
            for required in ["target", "node_modules", "vendor", "dist", "build", ".next", ".nuxt"] {
                if !cfg.indexing.exclude_dirs.iter().any(|d| d == required) {
                    println!("  ! '{}' is not excluded.", required);
                }
            }
        }
    }

    // 3. Binary in PATH
    let rag_in_path = std::process::Command::new("which")
        .arg("ragpilot")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    check("'ragpilot' binary in PATH", rag_in_path);

    // 4. Git repo
    check("Git repository", root.join(".git").exists());

    // 5. Git hooks
    let post_commit = root.join(".git/hooks/post-commit");
    let has_hook = post_commit.exists() && {
        std::fs::read_to_string(&post_commit)
            .map(|c| c.contains("ragpilot update"))
            .unwrap_or(false)
    };
    check("Git hooks installed (run 'ragpilot hooks')", has_hook);

    // 6. MCP registration. `init` writes `.mcp.json`; `.claude/settings.json`
    // is still honoured because older setups registered the server there.
    let registered_in = [".mcp.json", ".claude/settings.json"]
        .into_iter()
        .find(|rel| {
            std::fs::read_to_string(root.join(rel))
                .map(|c| c.contains("ragpilot") && c.contains("mcp-server"))
                .unwrap_or(false)
        });
    check(
        &format!(
            "Claude Code MCP registration ({})",
            registered_in.unwrap_or(".mcp.json")
        ),
        registered_in.is_some(),
    );

    // 7. Brain — summarised here so one `doctor` covers the whole install.
    if brain::exists() {
        println!("\n{}", "─── Brain ───────────────────────────────────────".bold());
        for finding in brain::doctor::checks(false).await {
            check(&finding.label, finding.ok);
        }
        println!("  Full detail: {}", "ragpilot brain doctor".bold());
    }

    println!("\n{}", "─── Quick Fix ──────────────────────────────────".bold());
    println!("  ragpilot init     Index the project");
    println!("  ragpilot hooks    Install git hooks");
    println!("  Or register the server by hand in .mcp.json:");
    println!(r#"    {{"mcpServers":{{"ragpilot":{{"type":"stdio","command":"ragpilot","args":["--mcp-server"]}}}}}}"#);

    Ok(())
}

/// Print where this project's data lives, as `key=value` lines.
///
/// Scripts (the benchmark among them) used to reach into `.rag/` directly.
/// Now that the layout is resolved rather than fixed, they ask instead.
fn cmd_paths() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let p = paths::ProjectPaths::resolve(&root);

    println!("root={}", p.root().display());
    println!("layout={}", if p.is_legacy() { "legacy" } else { "global" });
    println!("id={}", p.id().unwrap_or(""));
    println!("data_dir={}", p.data_dir().display());
    println!("config={}", p.config().display());
    println!("state={}", p.state().display());
    println!("stores_db={}", p.stores_db().display());
    println!("queries={}", p.queries().display());
    println!("data_root={}", paths::data_root().display());
    println!("registry={}", paths::registry_path().display());
    println!("brain_dir={}", brain::dir().display());

    // The collection needs the config; absent config, the id is what it will be.
    let collection = config::Config::load(&p.config())
        .map(|c| p.collection(c.qdrant.collection.as_deref(), &c.project.name))
        .unwrap_or_else(|_| p.id().unwrap_or("").to_string());
    println!("collection={collection}");
    Ok(())
}

/// Read `--flag value` or `--flag=value` from the argument list.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
}

fn check(label: &str, ok: bool) {
    use colored::Colorize;
    if ok {
        println!("  {}  {}", "✓".green(), label);
    } else {
        println!("  {}  {}", "✗".red(), label);
    }
}

// ─── rag setup ───────────────────────────────────────────────────────────────

async fn cmd_setup(args: &[String]) -> anyhow::Result<()> {
    use colored::Colorize;

    let folder = match args.get(2) {
        Some(f) => f.clone(),
        None => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: claude, codex, cursor, vscode, opencode, windsurf, antigravity, all"),
    };
    let agent = match args.get(3) {
        Some(a) => a.clone(),
        None => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: claude, codex, cursor, vscode, opencode, windsurf, antigravity, all"),
    };

    // Resolve absolute path
    let root = {
        let p = std::path::Path::new(&folder);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()?.join(p)
        }
    };

    // Create directory if needed
    let created = !root.exists();
    if created {
        std::fs::create_dir_all(&root)?;
    }

    // Canonicalize, register, and create `<data_root>/projects/<id>/`. From
    // here on `root` is the canonical path, so the MCP snippets and the
    // registry key agree with what the server resolves at startup.
    let project_paths = paths::register_project(&root)?;
    let root = project_paths.root().to_path_buf();
    if created {
        println!("{} Created directory: {}", "✓".green(), root.display());
    } else {
        println!("{} Directory: {}", "i".blue(), root.display());
    }
    if let Some(id) = project_paths.id() {
        println!("{} Project id: {}", "✓".green(), id.bold());
    }

    let project_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    // The project config lives in that data directory — nothing but the MCP
    // config and the agent markdown is written into the project folder.
    let config_path = project_paths.config();
    if !config_path.exists() {
        let choices = wizard::configure(&root);
        std::fs::write(
            &config_path,
            config::Config::template_with(&project_name, &choices.extensions, &choices.include_dirs),
        )?;
        println!("{} {}", "✓".green(), config_path.display());
        println!("    {} {}", "extensions:".dimmed(), choices.extensions.join(", "));
        let dirs = if choices.include_dirs.is_empty() {
            "(entire project root)".to_string()
        } else {
            choices.include_dirs.join(", ")
        };
        println!("    {} {}", "directories:".dimmed(), dirs);
    } else {
        println!("{} {} (already exists)", "i".blue(), config_path.display());
    }

    // Agent-specific MCP registration (claude, codex, cursor, vscode,
    // windsurf, antigravity, or "all").
    agents::configure(&agent, &root)?;

    // Switch cwd so cmd_init / cmd_hooks pick up the right root
    std::env::set_current_dir(&root)?;

    indexer::cmd_init(false).await?;

    if root.join(".git").exists() {
        println!("{} Installing git hooks…", "→".cyan());
        cmd_hooks().await?;
    } else {
        println!(
            "{} No .git found — skipping hooks. Run 'ragpilot hooks' after 'git init'.",
            "i".blue()
        );
    }

    println!("\n{} Setup complete!", "✓".green());
    println!("  Verify with: {}", "ragpilot doctor".bold());
    Ok(())
}

// ─── Static file content ──────────────────────────────────────────────────────

pub(crate) const AGENTS_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

Broad file scanning and large-context loading are forbidden in this project.
All discovery and analysis must go through the MCP server.

────────────────────────────────────────────────────

## 1. INDEX GUARANTEE

At the start of every task:

1. Call `rag_index_status`.
2. If `Dirty files > 0`:
   → Call `rag_ensure_index`.
3. Do not analyze until the index is up to date.

────────────────────────────────────────────────────

## 2. CONTEXT ACQUISITION RULE

At the start of a task:

→ Call `context_bundle(task, budget_tokens)`.

Do not open files manually.
If `rag_search` alone is not enough, prefer `context_bundle`.

Reading an entire file is forbidden.
If needed, use only:
→ `rag_get_file_ranges`
or
→ `rag_get_chunks`

────────────────────────────────────────────────────

## 3. SYMBOL NAVIGATION RULE

When you need information about a function/class:

1. `nav_symbol_resolve`
2. `nav_call_graph`

Do not make a refactor plan without producing the call graph.

────────────────────────────────────────────────────

## 4. REFACTOR SAFETY RULE

Before refactoring:

1. `impact_analyze`
2. Check breaking signals.
3. List the affected files.
4. Then make the change.

Refactoring without impact analysis is forbidden.

────────────────────────────────────────────────────

## 5. NO BROAD FILE READS

The following are forbidden:

✗ Scanning the whole repo
✗ Reading a large file in full
✗ Guessing dependencies

Always use the MCP tools.

────────────────────────────────────────────────────

## 6. TOKEN OPTIMIZATION PRIORITY

When gathering context:

- Maximum 6000 tokens (context_bundle default)
- No unnecessary repetition
- Do not repeat the same query

────────────────────────────────────────────────────

## 7. FALLBACK RULE

If the MCP server is unreachable:

- Notify the user
- Ask for approval before doing any manual file analysis

────────────────────────────────────────────────────
"#;

pub(crate) const CLAUDE_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

Broad file scanning and large-context loading are forbidden in this project.
All discovery and analysis must go through the `rag` MCP server.

## MCP Server

The `rag` MCP server is automatically active in this project.
It is registered in `.mcp.json`.

Available tools:

| Tool | Purpose |
|------|---------|
| `rag_index_status` | Index status and dirty file count |
| `rag_ensure_index` | Re-index changed files |
| `rag_search` | Semantic code search |
| `rag_get_chunks` | Fetch full content by chunk ID |
| `rag_get_file_ranges` | Specific line ranges or symbol definitions |
| `nav_symbol_resolve` | Symbol definition + call graph |
| `nav_call_graph` | BFS call tree (incoming + outgoing) |
| `impact_analyze` | Pre-refactor impact analysis |
| `context_bundle` | Token-budgeted complete context bundle |

────────────────────────────────────────────────────

## 1. INDEX GUARANTEE

At the start of every task:

1. Call `rag_index_status`.
2. If `Dirty files > 0`:
   → Call `rag_ensure_index`.
3. Do not analyze until the index is up to date.

────────────────────────────────────────────────────

## 2. CONTEXT ACQUISITION RULE

At the start of a task:

→ Call `context_bundle(task, budget_tokens)`.

Do not open files manually.
If `rag_search` alone is not enough, prefer `context_bundle`.

Reading an entire file is forbidden.
If needed, use only:
→ `rag_get_file_ranges`
or
→ `rag_get_chunks`

────────────────────────────────────────────────────

## 3. SYMBOL NAVIGATION RULE

When you need information about a function/class:

1. `nav_symbol_resolve`
2. `nav_call_graph`

Do not make a refactor plan without producing the call graph.

────────────────────────────────────────────────────

## 4. REFACTOR SAFETY RULE

Before refactoring:

1. `impact_analyze`
2. Check breaking signals.
3. List the affected files.
4. Then make the change.

Refactoring without impact analysis is forbidden.

────────────────────────────────────────────────────

## 5. NO BROAD FILE READS

The following are forbidden:

✗ Scanning the whole repo
✗ Reading a large file in full
✗ Guessing dependencies

Always use the MCP tools.

────────────────────────────────────────────────────

## 6. TOKEN OPTIMIZATION PRIORITY

When gathering context:

- Maximum 6000 tokens (context_bundle default)
- No unnecessary repetition
- Do not repeat the same query

────────────────────────────────────────────────────

## 7. FALLBACK RULE

If the MCP server is unreachable:

- Notify the user
- Ask for approval before doing any manual file analysis

────────────────────────────────────────────────────
"#;

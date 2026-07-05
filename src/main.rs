mod config;
mod embedder;
mod store;
mod indexer;
mod parser;
mod skeleton;
mod tokens;
mod orchestrator;
mod watcher;
mod mcp;
mod wizard;
mod semantic_diff;
mod agents;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("ragpilot {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }

        Some("--mcp-server") => mcp::run_server().await,

        Some("init") => {
            // "ragpilot init <folder> <agent>"  →  setup mode
            // "ragpilot init [--force]"         →  index only
            let has_folder = args.get(2).map(|a| !a.starts_with('-')).unwrap_or(false);
            let has_agent  = args.get(3).map(|a| !a.starts_with('-')).unwrap_or(false);
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
                   ragpilot update                 Re-index changed files\n\
                   ragpilot status                 Show index statistics\n\
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
                 MCP registration (.claude/settings.json):\n\
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

    let root        = std::env::current_dir()?;
    let config_path = config::Config::config_path(&root);
    let state_path  = config::Config::state_path(&root);
    let stores_path = config::Config::stores_db(&root);

    println!("{}", "─── ragpilot doctor ────────────────────────────".bold());

    // 1. Config
    check("Config file exists",   config_path.exists());
    check("State file exists",    state_path.exists());
    check("SQLite stores exist",  stores_path.exists());

    // 2. Qdrant connectivity
    if config_path.exists() {
        if let Ok(cfg) = config::Config::load(&config_path) {
            let qdrant_ok = tokio::task::spawn_blocking({
                let url = cfg.qdrant.url.clone();
                move || {
                    let client = qdrant_client::Qdrant::from_url(&url).build();
                    client.is_ok()
                }
            }).await.unwrap_or(false);
            check(&format!("Qdrant reachable ({})", cfg.qdrant.url), qdrant_ok);

            // Offline readiness: with the local provider, the embedding model
            // must already be in the cache for air-gapped operation.
            if cfg.embedding.provider == "local" {
                let cache = embedder::local::resolve_cache_dir(&cfg.embedding.local, &root);
                let cached = embedder::local::cache_has_model(&cache);
                check(
                    &format!("Embedding model cached ({})", cache.display()),
                    cached,
                );
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

    // 6. MCP registration
    let mcp_settings = root.join(".claude/settings.json");
    let mcp_ok = mcp_settings.exists() && {
        std::fs::read_to_string(&mcp_settings)
            .map(|c| c.contains("ragpilot") && c.contains("mcp-server"))
            .unwrap_or(false)
    };
    check("Claude Code MCP registration (.claude/settings.json)", mcp_ok);

    println!("\n{}", "─── Quick Fix ──────────────────────────────────".bold());
    println!("  ragpilot init     Index the project");
    println!("  ragpilot hooks    Install git hooks");
    println!("  Add to .claude/settings.json:");
    println!(r#"    {{"mcpServers":{{"ragpilot":{{"type":"stdio","command":"ragpilot","args":["--mcp-server"]}}}}}}"#);

    Ok(())
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
        None    => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: claude, codex, cursor, vscode, opencode, windsurf, antigravity, all"),
    };
    let agent = match args.get(3) {
        Some(a) => a.clone(),
        None    => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: claude, codex, cursor, vscode, opencode, windsurf, antigravity, all"),
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
    if !root.exists() {
        std::fs::create_dir_all(&root)?;
        println!("{} Created directory: {}", "✓".green(), root.display());
    } else {
        println!("{} Directory: {}", "i".blue(), root.display());
    }

    let project_name = root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    // .rag/config.toml
    let rag_dir     = root.join(".rag");
    let config_path = rag_dir.join("config.toml");
    std::fs::create_dir_all(&rag_dir)?;
    if !config_path.exists() {
        let choices = wizard::configure(&root);
        std::fs::write(
            &config_path,
            config::Config::template_with(&project_name, &choices.extensions, &choices.include_dirs),
        )?;
        println!("{} .rag/config.toml", "✓".green());
        println!("    {} {}", "extensions:".dimmed(), choices.extensions.join(", "));
        let dirs = if choices.include_dirs.is_empty() {
            "(entire project root)".to_string()
        } else {
            choices.include_dirs.join(", ")
        };
        println!("    {} {}", "directories:".dimmed(), dirs);
    } else {
        println!("{} .rag/config.toml (already exists)", "i".blue());
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


const AGENTS_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

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


const CLAUDE_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

Broad file scanning and large-context loading are forbidden in this project.
All discovery and analysis must go through the `rag` MCP server.

## MCP Server

The `rag` MCP server is automatically active in this project.
It is registered in `.claude/settings.json`.

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

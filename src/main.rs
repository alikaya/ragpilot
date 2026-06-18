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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("--mcp-server") => mcp::run_server().await,

        Some("init") => {
            // "ragpilot init <folder> <agent>"  →  setup modu
            // "ragpilot init [--force]"         →  sadece indexleme
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
                   ragpilot init <folder> <agent>     Init project + agent config (codex|claude)\n\
                   ragpilot init [--force]            Index current project\n\
                   ragpilot setup <folder> <agent>    Alias for 'ragpilot init <folder> <agent>'\n\
                   ragpilot update                 Re-index changed files\n\
                   ragpilot status                 Show index statistics\n\
\n\
                   ragpilot stats                  Show last context.bundle token savings\n\
                   ragpilot skeleton <file>        Print a token-efficient skeleton of a file\n\
\n\
                   ragpilot clean [--yes]          Delete Qdrant collection\n\
                   ragpilot hooks                  Install git post-commit/post-merge hooks\n\
                   ragpilot doctor                 Check installation and configuration\n\
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

            println!("\n{}", "─── Resource Warnings ───────────────────────────".bold());
            if cfg.indexing.include_dirs.is_empty() {
                println!("  ! Tüm proje indexlenecek, büyük projelerde kaynak tüketimi artabilir.");
            }
            if cfg.indexing.include_extensions.len() > 8 {
                println!("  ! Çok fazla dosya tipi indexleniyor.");
            }
            if cfg.indexing.max_file_size_kb > 500 {
                println!("  ! Büyük dosyalar RAM/CPU tüketimini artırabilir.");
            }
            if cfg.mcp.context_chunks > 6 {
                println!("  ! MCP context sonuçları gereğinden büyük olabilir.");
            }
            if cfg.watcher.enabled {
                println!("  ! Watcher büyük projelerde sürekli re-index tetikleyebilir.");
            }
            for required in ["target", "node_modules", "vendor", "dist", "build", ".next", ".nuxt"] {
                if !cfg.indexing.exclude_dirs.iter().any(|d| d == required) {
                    println!("  ! '{}' exclude edilmemiş.", required);
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
            .map(|c| c.contains("rag") && c.contains("mcp-server"))
            .unwrap_or(false)
    };
    check("Claude Code MCP registration (.claude/settings.json)", mcp_ok);

    println!("\n{}", "─── Quick Fix ──────────────────────────────────".bold());
    println!("  ragpilot init     Index the project");
    println!("  ragpilot hooks    Install git hooks");
    println!("  Add to .claude/settings.json:");
    println!(r#"    {{"mcpServers":{{"rag":{{"type":"stdio","command":"ragpilot","args":["--mcp-server"]}}}}}}"#);

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
        None    => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: codex, claude"),
    };
    let agent = match args.get(3) {
        Some(a) => a.clone(),
        None    => anyhow::bail!("Usage: ragpilot setup <folder> <agent>\n  Agents: codex, claude"),
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
        std::fs::write(&config_path, config::Config::default_template(&project_name))?;
        println!("{} .rag/config.toml", "✓".green());
    } else {
        println!("{} .rag/config.toml (already exists)", "i".blue());
    }

    // Agent-specific config files
    match agent.to_lowercase().as_str() {
        "codex"  => write_codex_files(&root)?,
        "claude" => write_claude_files(&root)?,
        other => anyhow::bail!(
            "Unknown agent '{}'. Supported agents: codex, claude",
            other
        ),
    }

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

fn write_codex_files(root: &std::path::Path) -> anyhow::Result<()> {
    use colored::Colorize;

    // .codex/config.toml — proje yolu dinamik olarak eklenir
    let codex_dir    = root.join(".codex");
    let codex_config = codex_dir.join("config.toml");
    std::fs::create_dir_all(&codex_dir)?;
    if !codex_config.exists() {
        let root_str = root.canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .to_string();
        let content = format!(
            "[projects.\"{}\"]\ntrust_level = \"trusted\"\n\n\
             [mcp_servers.rag]\ncommand = \"ragpilot\"\nargs    = [\"--mcp-server\"]\n\n\
             # Güvenlik için sadece bu projede aktif\ntrusted = true\n",
            root_str
        );
        std::fs::write(&codex_config, content)?;
        println!("{} .codex/config.toml", "✓".green());
    } else {
        println!("{} .codex/config.toml (already exists)", "i".blue());
    }

    // AGENTS.md
    let agents_md = root.join("AGENTS.md");
    if !agents_md.exists() {
        std::fs::write(&agents_md, AGENTS_MD)?;
        println!("{} AGENTS.md", "✓".green());
    } else {
        println!("{} AGENTS.md (already exists)", "i".blue());
    }

    Ok(())
}

// ─── Static file content ──────────────────────────────────────────────────────

fn write_claude_files(root: &std::path::Path) -> anyhow::Result<()> {
    use colored::Colorize;

    // .mcp.json — merge if exists, create if not
    let mcp_json_path = root.join(".mcp.json");
    let rag_entry = serde_json::json!({
        "type":    "stdio",
        "command": "ragpilot",
        "args":    ["--mcp-server"]
    });

    if mcp_json_path.exists() {
        let raw = std::fs::read_to_string(&mcp_json_path)?;
        let mut json: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|_| serde_json::json!({}));

        if json.pointer("/mcpServers/rag").is_some() {
            println!("{} .mcp.json (rag already registered)", "i".blue());
        } else {
            json["mcpServers"]["rag"] = rag_entry;
            std::fs::write(&mcp_json_path, serde_json::to_string_pretty(&json)?)?;
            println!("{} .mcp.json (rag MCP server added)", "✓".green());
        }
    } else {
        let content = serde_json::json!({
            "mcpServers": { "rag": rag_entry }
        });
        std::fs::write(&mcp_json_path, serde_json::to_string_pretty(&content)?)?;
        println!("{} .mcp.json", "✓".green());
    }

    // CLAUDE.md
    let claude_md = root.join("CLAUDE.md");
    if !claude_md.exists() {
        std::fs::write(&claude_md, CLAUDE_MD)?;
        println!("{} CLAUDE.md", "✓".green());
    } else {
        println!("{} CLAUDE.md (already exists)", "i".blue());
    }

    Ok(())
}


const AGENTS_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

Bu projede dosya tarama ve geniş bağlam yükleme YASAKTIR.
Tüm keşif ve analiz işlemleri MCP üzerinden yapılmalıdır.

────────────────────────────────────────────────────

## 1. INDEX GUARANTEE

Her görev başlangıcında:

1. `rag.index_status` çağır.
2. Eğer `Dirty files > 0` ise:
   → `rag.ensure_index` çağır.
3. Index güncel olmadan analiz yapma.

────────────────────────────────────────────────────

## 2. CONTEXT ACQUISITION RULE

Görev başında:

→ `context.bundle(task, budget_tokens)` çağır.

Dosyaları manuel açma.
`rag.search` tek başına yeterli değilse `context.bundle` tercih edilir.

Dosya tamamını okumak YASAKTIR.
Gerekirse sadece:
→ `rag.get_file_ranges`
veya
→ `rag.get_chunks`
kullanılabilir.

────────────────────────────────────────────────────

## 3. SYMBOL NAVIGATION RULE

Bir fonksiyon/class hakkında bilgi gerekiyorsa:

1. `nav.symbol_resolve`
2. `nav.call_graph`

Çağrı grafiği çıkarılmadan refactor planı yapılmaz.

────────────────────────────────────────────────────

## 4. REFACTOR SAFETY RULE

Refactor yapılacaksa:

1. `impact.analyze`
2. Breaking signals kontrol edilir.
3. Etkilenen dosyalar listelenir.
4. Ardından değişiklik yapılır.

Impact analizi olmadan refactor YASAKTIR.

────────────────────────────────────────────────────

## 5. NO BROAD FILE READS

Aşağıdakiler yasaktır:

✗ Tüm repo tarama
✗ Büyük dosyayı komple okuma
✗ Tahmini dependency çıkarma

Her zaman MCP araçları kullanılmalıdır.

────────────────────────────────────────────────────

## 6. TOKEN OPTIMIZATION PRIORITY

Bağlam toplarken:

- Maksimum 6000 token (context.bundle default)
- Gereksiz tekrar yok
- Aynı sorgu tekrar edilmez

────────────────────────────────────────────────────

## 7. FALLBACK RULE

MCP sunucusu erişilemezse:

- Kullanıcıya bildir
- Manuel dosya analizi yapmadan önce onay iste

────────────────────────────────────────────────────
"#;


const CLAUDE_MD: &str = r#"# AGENT EXECUTION POLICY — RAG-FIRST

Bu projede dosya tarama ve geniş bağlam yükleme YASAKTIR.
Tüm keşif ve analiz işlemleri `rag` MCP sunucusu üzerinden yapılmalıdır.

## MCP Sunucusu

`rag` MCP sunucusu bu projede otomatik olarak aktiftir.
`.claude/settings.json` içinde kayıtlıdır.

Mevcut araçlar:

| Araç | Amaç |
|------|------|
| `rag.index_status` | Index durumu ve dirty dosya sayısı |
| `rag.ensure_index` | Değişen dosyaları yeniden indexle |
| `rag.search` | Semantik kod arama |
| `rag.get_chunks` | Chunk ID ile tam içerik getir |
| `rag.get_file_ranges` | Belirli satır aralıkları veya sembol tanımları |
| `nav.symbol_resolve` | Sembol tanımı + çağrı grafı |
| `nav.call_graph` | BFS çağrı ağacı (gelen + giden) |
| `impact.analyze` | Refactor öncesi etki analizi |
| `context.bundle` | Token bütçeli eksiksiz bağlam paketi |

────────────────────────────────────────────────────

## 1. INDEX GUARANTEE

Her görev başlangıcında:

1. `rag.index_status` çağır.
2. Eğer `Dirty files > 0` ise:
   → `rag.ensure_index` çağır.
3. Index güncel olmadan analiz yapma.

────────────────────────────────────────────────────

## 2. CONTEXT ACQUISITION RULE

Görev başında:

→ `context.bundle(task, budget_tokens)` çağır.

Dosyaları manuel açma.
`rag.search` tek başına yeterli değilse `context.bundle` tercih edilir.

Dosya tamamını okumak YASAKTIR.
Gerekirse sadece:
→ `rag.get_file_ranges`
veya
→ `rag.get_chunks`
kullanılabilir.

────────────────────────────────────────────────────

## 3. SYMBOL NAVIGATION RULE

Bir fonksiyon/class hakkında bilgi gerekiyorsa:

1. `nav.symbol_resolve`
2. `nav.call_graph`

Çağrı grafiği çıkarılmadan refactor planı yapılmaz.

────────────────────────────────────────────────────

## 4. REFACTOR SAFETY RULE

Refactor yapılacaksa:

1. `impact.analyze`
2. Breaking signals kontrol edilir.
3. Etkilenen dosyalar listelenir.
4. Ardından değişiklik yapılır.

Impact analizi olmadan refactor YASAKTIR.

────────────────────────────────────────────────────

## 5. NO BROAD FILE READS

Aşağıdakiler yasaktır:

✗ Tüm repo tarama
✗ Büyük dosyayı komple okuma
✗ Tahmini dependency çıkarma

Her zaman MCP araçları kullanılmalıdır.

────────────────────────────────────────────────────

## 6. TOKEN OPTIMIZATION PRIORITY

Bağlam toplarken:

- Maksimum 6000 token (context.bundle default)
- Gereksiz tekrar yok
- Aynı sorgu tekrar edilmez

────────────────────────────────────────────────────

## 7. FALLBACK RULE

MCP sunucusu erişilemezse:

- Kullanıcıya bildir
- Manuel dosya analizi yapmadan önce onay iste

────────────────────────────────────────────────────
"#;

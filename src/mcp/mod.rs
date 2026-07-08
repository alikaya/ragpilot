pub mod protocol;
pub mod tools;

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::embedder;
use crate::store::qdrant::QdrantStore;
use crate::store::impact_index::ImpactIndexStore;
use crate::store::project_tree::ProjectTreeStore;
use crate::store::symbol_graph::SymbolGraphStore;
use crate::orchestrator::IndexOrchestrator;
use crate::watcher::FileWatcher;
use protocol::{McpRequest, McpResponse};
use tools::McpContext;

/// Minimal context handed to an observer: which project and where it lives.
/// Deliberately narrow — an observer sees identity, never the stores.
pub struct ObserverContext<'a> {
    pub project: &'a str,
    pub root: &'a std::path::Path,
}

/// A generic seam invoked after each MCP exchange, once the response has
/// already been sent to the client. The open-source core ships no observer; a
/// separate build can supply one (e.g. usage/audit reporting). Implementations
/// must be cheap and non-blocking — this runs on the request path.
pub trait ToolObserver: Send + Sync {
    fn observe(&self, ctx: &ObserverContext, request: &McpRequest, response: &McpResponse);
}

pub async fn run_server() -> anyhow::Result<()> {
    run_server_with(None).await
}

pub async fn run_server_with(observer: Option<Arc<dyn ToolObserver>>) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAG_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))
        )
        .with_target(false)
        .init();

    // Project-root resolution, in priority order:
    //   1. an explicit `--root <path>` / `RAGPILOT_ROOT` (for global, folder-
    //      independent clients such as Antigravity/Windsurf),
    //   2. the client's workspace root announced during `initialize` (below),
    //   3. the current working directory (unchanged path for project-scoped
    //      clients launched inside the project — claude/codex/cursor/…).
    let explicit_root = resolve_explicit_root();

    // Eager build: an explicit root, or a cwd that already has a config. Missing
    // config no longer aborts the process — we start regardless and answer the
    // handshake, so the client never sees an "EOF" mid-initialize.
    let eager_root = explicit_root.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .filter(|cwd| Config::config_path(cwd).exists())
    });

    let mut ctx: Option<Arc<McpContext>> = None;
    if let Some(r) = eager_root {
        match build_context(&r).await {
            Ok(c)  => ctx = Some(c),
            Err(e) => eprintln!("ragpilot: could not load project at {}: {e}", r.display()),
        }
    }
    if ctx.is_none() {
        eprintln!(
            "ragpilot: no project loaded yet — waiting for the client to announce a \
             workspace root, or pass --root <path> / set RAGPILOT_ROOT."
        );
    }

    // ── stdio JSON-RPC loop ────────────────────────────────────────────────
    let stdin      = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        tracing::debug!("← {}", trimmed);

        let request: McpRequest = match serde_json::from_str(trimmed) {
            Ok(r)  => r,
            Err(e) => {
                let msg = McpResponse::error(-32700, &format!("Parse error: {e}"), None);
                write_response(&mut stdout, &msg).await?;
                continue;
            }
        };

        if request.is_notification() {
            tracing::debug!("← notification: {}", request.method);
            continue;
        }

        // Lazily adopt the client's workspace root from the initialize handshake
        // — only when no project is loaded yet and no explicit root was pinned.
        if ctx.is_none() && explicit_root.is_none() && request.method == "initialize" {
            if let Some(r) = workspace_root_from_initialize(&request) {
                if Config::config_path(&r).exists() {
                    match build_context(&r).await {
                        Ok(c)  => {
                            eprintln!("ragpilot: loaded project from client workspace {}", r.display());
                            ctx = Some(c);
                        }
                        Err(e) => eprintln!("ragpilot: could not load workspace {}: {e}", r.display()),
                    }
                }
            }
        }

        let response = tools::handle_request(&request, ctx.as_ref()).await;
        write_response(&mut stdout, &response).await?;

        // Generic observation seam, strictly after the response is sent so it
        // can never slow or fail a tool call. No-op unless a build supplied one.
        if let (Some(obs), Some(c)) = (observer.as_ref(), ctx.as_ref()) {
            obs.observe(
                &ObserverContext { project: &c.config.project.name, root: &c.root },
                &request,
                &response,
            );
        }
    }

    Ok(())
}

/// Build the full server context (config + embedder + stores + orchestrator, and
/// start the watcher when enabled) for a given project root. Fails if the root
/// has no readable `.rag/config.toml` or a store cannot be opened.
async fn build_context(root: &std::path::Path) -> anyhow::Result<Arc<McpContext>> {
    let config_path = Config::config_path(root);
    let config = Arc::new(Config::load(&config_path)?);
    tracing::info!("Loaded config for '{}'", config.project.name);

    let model_name = match config.embedding.provider.as_str() {
        "api" => config.embedding.api.model.as_str(),
        _     => config.embedding.local.model.as_str(),
    };
    eprintln!("ragpilot: loading embedding model '{model_name}'…");
    let embedder: Arc<dyn embedder::Embedder> = Arc::from(embedder::create(&config.embedding, root)?);
    eprintln!("ragpilot: model ready. MCP server running on stdio.");

    // Vector store
    let collection = config.qdrant.collection_name(&config.project.name);
    let mut qdrant_cfg  = config.qdrant.clone();
    qdrant_cfg.collection = Some(collection);
    let vector_store: Arc<dyn crate::store::VectorStore> = Arc::new(QdrantStore::new(&qdrant_cfg)?);

    // SQLite stores (all share same .rag/stores.db)
    let db_path = Config::stores_db(root);
    crate::store::sqlite::SqliteStore::new(db_path.clone())?; // ensure schema exists
    let symbol_graph: Arc<SymbolGraphStore> = Arc::new(SymbolGraphStore::new(db_path.clone()));
    let project_tree: Arc<ProjectTreeStore> = Arc::new(ProjectTreeStore::new(db_path.clone()));
    let impact_index: Arc<ImpactIndexStore> = Arc::new(ImpactIndexStore::new(db_path.clone()));

    // Orchestrator
    let orchestrator: Arc<IndexOrchestrator> = Arc::new(IndexOrchestrator::new(
        Arc::clone(&config),
        root.to_path_buf(),
        Arc::clone(&embedder),
        Arc::clone(&vector_store),
        Arc::clone(&symbol_graph),
        Arc::clone(&project_tree),
        Arc::clone(&impact_index),
    ));

    // Start file watcher if enabled
    if config.watcher.enabled {
        let orch = Arc::clone(&orchestrator);
        let r    = root.to_path_buf();
        let dms  = config.watcher.debounce_ms;
        tokio::spawn(async move {
            FileWatcher::start(r, orch, dms).await;
        });
    }

    Ok(Arc::new(McpContext {
        config,
        root: root.to_path_buf(),
        embedder,
        store: vector_store,
        symbol_graph,
        project_tree,
        impact_index,
        orchestrator,
    }))
}

/// Resolve an explicitly-pinned project root from `--root <path>` / `--root=<path>`
/// (or the short `-r`), falling back to the `RAGPILOT_ROOT` environment variable.
/// Canonicalised when possible so relative launches still resolve correctly.
fn resolve_explicit_root() -> Option<std::path::PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let mut raw: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--root" || a == "-r" {
            raw = args.get(i + 1).cloned();
            break;
        } else if let Some(v) = a.strip_prefix("--root=") {
            raw = Some(v.to_string());
            break;
        }
        i += 1;
    }
    let raw = raw.or_else(|| std::env::var("RAGPILOT_ROOT").ok())?;
    let p = std::path::PathBuf::from(raw);
    Some(p.canonicalize().unwrap_or(p))
}

/// Best-effort extraction of the client's workspace root from the `initialize`
/// params. Supports the common shapes clients send: `rootUri` (file:// URI),
/// `workspaceFolders[0].uri`, and the legacy `rootPath`.
fn workspace_root_from_initialize(req: &McpRequest) -> Option<std::path::PathBuf> {
    let params = req.params.as_ref()?;
    let uri = params
        .get("rootUri")
        .and_then(|v| v.as_str())
        .or_else(|| {
            params
                .get("workspaceFolders")
                .and_then(|w| w.as_array())
                .and_then(|arr| arr.first())
                .and_then(|f| f.get("uri"))
                .and_then(|v| v.as_str())
        });
    if let Some(u) = uri {
        if let Some(p) = file_uri_to_path(u) {
            return Some(p);
        }
    }
    params
        .get("rootPath")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
}

/// Convert a `file://` URI (or a bare absolute path) into a filesystem path.
/// Minimal percent-decoding for the common space case; returns None otherwise.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    if let Some(rest) = uri.strip_prefix("file://") {
        // Drop an optional authority component: file://host/path → /path.
        let path = match rest.find('/') {
            Some(idx) => &rest[idx..],
            None      => rest,
        };
        Some(std::path::PathBuf::from(path.replace("%20", " ")))
    } else if uri.starts_with('/') {
        Some(std::path::PathBuf::from(uri))
    } else {
        None
    }
}

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    resp:   &McpResponse,
) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(resp)?;
    line.push('\n');
    tracing::debug!("→ {}", line.trim());
    stdout.write_all(line.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

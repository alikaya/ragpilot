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

pub async fn run_server() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAG_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))
        )
        .with_target(false)
        .init();

    let root = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Cannot determine working directory: {e}"))?;

    let config_path = Config::config_path(&root);
    if !config_path.exists() {
        eprintln!(
            "ragpilot MCP: config not found at {}.\nRun 'ragpilot init' first.",
            config_path.display()
        );
        std::process::exit(1);
    }

    let config = Arc::new(Config::load(&config_path)?);
    tracing::info!("Loaded config for '{}'", config.project.name);

    let model_name = match config.embedding.provider.as_str() {
        "api" => config.embedding.api.model.as_str(),
        _     => config.embedding.local.model.as_str(),
    };
    eprintln!("ragpilot: loading embedding model '{model_name}'…");
    let embedder: Arc<dyn embedder::Embedder> = Arc::from(embedder::create(&config.embedding)?);
    eprintln!("ragpilot: model ready. MCP server running on stdio.");

    // Vector store
    let collection = config.qdrant.collection_name(&config.project.name);
    let mut qdrant_cfg  = config.qdrant.clone();
    qdrant_cfg.collection = Some(collection);
    let vector_store: Arc<dyn crate::store::VectorStore> = Arc::new(QdrantStore::new(&qdrant_cfg)?);

    // SQLite stores (all share same .rag/stores.db)
    let db_path     = Config::stores_db(&root);
    crate::store::sqlite::SqliteStore::new(db_path.clone())?; // ensure schema exists
    let symbol_graph: Arc<SymbolGraphStore> = Arc::new(SymbolGraphStore::new(db_path.clone()));
    let project_tree: Arc<ProjectTreeStore> = Arc::new(ProjectTreeStore::new(db_path.clone()));
    let impact_index: Arc<ImpactIndexStore> = Arc::new(ImpactIndexStore::new(db_path.clone()));

    // Orchestrator
    let orchestrator: Arc<IndexOrchestrator> = Arc::new(IndexOrchestrator::new(
        Arc::clone(&config),
        root.clone(),
        Arc::clone(&embedder),
        Arc::clone(&vector_store),
        Arc::clone(&symbol_graph),
        Arc::clone(&project_tree),
        Arc::clone(&impact_index),
    ));

    // Start file watcher if enabled
    if config.watcher.enabled {
        let orch   = Arc::clone(&orchestrator);
        let r      = root.clone();
        let dms    = config.watcher.debounce_ms;
        tokio::spawn(async move {
            FileWatcher::start(r, orch, dms).await;
        });
    }

    let ctx = Arc::new(McpContext {
        config:       Arc::clone(&config),
        root,
        embedder,
        store:        vector_store,
        symbol_graph,
        project_tree,
        impact_index,
        orchestrator,
    });

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

        let response = tools::handle_request(&request, &ctx).await;
        write_response(&mut stdout, &response).await?;
    }

    Ok(())
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

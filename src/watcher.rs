use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::orchestrator::IndexOrchestrator;

pub struct FileWatcher;

impl FileWatcher {
    /// Start watching `root` for changes. Runs forever — spawn as a background task.
    pub async fn start(root: PathBuf, orchestrator: Arc<IndexOrchestrator>, debounce_ms: u64) {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<notify::Event>>(256);

        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let _ = tx.blocking_send(res);
        }) {
            Ok(w)  => w,
            Err(e) => { tracing::error!("Cannot create file watcher: {e}"); return; }
        };

        use notify::{RecursiveMode, Watcher};
        if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
            tracing::error!("Cannot watch {}: {e}", root.display());
            return;
        }

        eprintln!("ragpilot: watching {} for changes…", root.display());

        let debounce = Duration::from_millis(debounce_ms);
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        Ok(ev) => {
                            use notify::EventKind::*;
                            match ev.kind {
                                Create(_) | Modify(_) | Remove(_) => {
                                    for path in ev.paths {
                                        if orchestrator.should_index(&path) {
                                            pending.insert(path, Instant::now());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => tracing::warn!("Watch error: {e}"),
                    }
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    let ready: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, t)| now.duration_since(**t) >= debounce)
                        .map(|(p, _)| p.clone())
                        .collect();

                    for path in ready {
                        pending.remove(&path);
                        let orch = Arc::clone(&orchestrator);
                        tokio::spawn(async move {
                            if let Err(e) = orch.process_file(&path).await {
                                tracing::warn!("process_file {}: {e}", path.display());
                            }
                        });
                    }
                }
            }
        }
        // watcher is implicitly kept alive until this function returns (never)
        #[allow(unreachable_code)]
        drop(watcher);
    }
}

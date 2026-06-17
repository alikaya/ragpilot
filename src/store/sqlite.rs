use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Thin wrapper around a SQLite file path. Opens a new WAL-mode connection
/// per operation — cheap and safe for our async + tokio context.
pub struct SqliteStore {
    pub db_path: PathBuf,
}

impl SqliteStore {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create dir {}", parent.display()))?;
        }
        let store = Self { db_path };
        store.init_schema()?;
        Ok(store)
    }

    pub fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("SQLite open: {}", self.db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;"
        )?;
        Ok(conn)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(SCHEMA)?;
        Ok(())
    }
}

const SCHEMA: &str = "
-- SymbolGraph
CREATE TABLE IF NOT EXISTS symbols (
    id          TEXT PRIMARY KEY,
    path        TEXT NOT NULL,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    start_line  INTEGER NOT NULL DEFAULT 1,
    end_line    INTEGER NOT NULL DEFAULT 1,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_path ON symbols(path);

CREATE TABLE IF NOT EXISTS symbol_calls (
    caller_id   TEXT NOT NULL,
    callee_name TEXT NOT NULL,
    call_line   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (caller_id, callee_name, call_line)
);

CREATE TABLE IF NOT EXISTS symbol_imports (
    importer    TEXT NOT NULL,
    from_module TEXT NOT NULL,
    symbol_name TEXT NOT NULL,
    PRIMARY KEY (importer, from_module, symbol_name)
);
CREATE INDEX IF NOT EXISTS idx_imports_module ON symbol_imports(from_module);

-- ProjectTree
CREATE TABLE IF NOT EXISTS tree_nodes (
    path        TEXT PRIMARY KEY,
    parent      TEXT,
    node_type   TEXT NOT NULL DEFAULT 'file',
    language    TEXT NOT NULL DEFAULT '',
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    hash        TEXT NOT NULL DEFAULT '',
    depth       INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tree_parent ON tree_nodes(parent);

-- ImpactIndex: reverse deps (who imports what from where)
CREATE TABLE IF NOT EXISTS dependents (
    imported_path   TEXT NOT NULL,
    dependent_path  TEXT NOT NULL,
    PRIMARY KEY (imported_path, dependent_path)
);
CREATE INDEX IF NOT EXISTS idx_dep_imported ON dependents(imported_path);
";

/// Open a connection to a db at `path` — helper used by the per-store modules.
pub fn open_conn(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;"
    )?;
    Ok(conn)
}

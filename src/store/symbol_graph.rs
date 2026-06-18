use anyhow::Result;
use rusqlite::params;
use std::path::PathBuf;

use crate::parser::{CallRef, Import, Symbol};
use super::sqlite::open_conn;

pub struct SymbolGraphStore {
    db_path: PathBuf,
}

/// Aggregate counts for the `status` dashboard.
#[derive(Debug, Default)]
pub struct GraphStats {
    pub total_symbols: usize,
    pub total_calls:   usize,
    pub total_imports: usize,
    /// (kind, count) ordered by count desc.
    pub by_kind:     Vec<(String, usize)>,
    /// Most-called project symbols: (name, incoming call count) desc.
    pub hot_symbols: Vec<(String, usize)>,
    /// Files with the most defined symbols: (path, count) desc.
    pub top_files:   Vec<(String, usize)>,
}

impl SymbolGraphStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    /// Replace all symbols, imports, and calls for a file.
    pub async fn upsert(
        &self,
        path: &str,
        symbols: &[Symbol],
        imports: &[Import],
        calls:   &[CallRef],
    ) -> Result<()> {
        let path    = path.to_string();
        let symbols = symbols.to_vec();
        let imports = imports.to_vec();
        let calls   = calls.to_vec();
        let db_path = self.db_path.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;

            // Delete existing data for this path
            conn.execute("DELETE FROM symbols       WHERE path     = ?1", params![path])?;
            conn.execute("DELETE FROM symbol_imports WHERE importer = ?1", params![path])?;
            conn.execute("DELETE FROM symbol_calls   WHERE caller_id LIKE ?1", params![format!("{}::%", path)])?;

            for s in &symbols {
                conn.execute(
                    "INSERT OR REPLACE INTO symbols
                     (id, path, name, kind, start_line, end_line, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
                    params![s.id, s.path, s.name, s.kind, s.start_line as i64, s.end_line as i64],
                )?;
            }

            for imp in &imports {
                conn.execute(
                    "INSERT OR REPLACE INTO symbol_imports (importer, from_module, symbol_name)
                     VALUES (?1, ?2, ?3)",
                    params![imp.importer, imp.from_module, imp.symbol_name],
                )?;
            }

            for call in &calls {
                conn.execute(
                    "INSERT OR REPLACE INTO symbol_calls (caller_id, callee_name, call_line)
                     VALUES (?1, ?2, ?3)",
                    params![call.caller_id, call.callee_name, call.call_line as i64],
                )?;
            }

            Ok(())
        }).await??;

        Ok(())
    }

    /// Remove all symbols, imports, and calls for a deleted/moved file.
    pub async fn remove(&self, path: &str) -> Result<()> {
        let path    = path.to_string();
        let db_path = self.db_path.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;
            conn.execute("DELETE FROM symbols        WHERE path     = ?1", params![path])?;
            conn.execute("DELETE FROM symbol_imports WHERE importer = ?1", params![path])?;
            conn.execute("DELETE FROM symbol_calls   WHERE caller_id LIKE ?1", params![format!("{}::%", path)])?;
            Ok(())
        }).await??;

        Ok(())
    }

    /// Drop every file's data whose path is not in `keep` (self-heals orphans
    /// left behind when the index state and the graph diverge). Returns the
    /// number of files pruned.
    pub async fn prune_except(&self, keep: Vec<String>) -> Result<usize> {
        let db_path = self.db_path.clone();

        let removed = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = open_conn(&db_path)?;
            let keep: std::collections::HashSet<String> = keep.into_iter().collect();

            let paths: Vec<String> = {
                let mut stmt = conn.prepare("SELECT DISTINCT path FROM symbols")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };

            let mut removed = 0;
            for p in paths {
                if !keep.contains(&p) {
                    conn.execute("DELETE FROM symbols        WHERE path     = ?1", params![p])?;
                    conn.execute("DELETE FROM symbol_imports WHERE importer = ?1", params![p])?;
                    conn.execute("DELETE FROM symbol_calls   WHERE caller_id LIKE ?1", params![format!("{}::%", p)])?;
                    removed += 1;
                }
            }
            Ok(removed)
        }).await??;

        Ok(removed)
    }

    /// Look up all symbols matching a name (exact, case-insensitive).
    pub async fn resolve(&self, name: &str) -> Result<Vec<Symbol>> {
        let name    = name.to_string();
        let db_path = self.db_path.clone();

        let symbols = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Symbol>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT id, path, name, kind, start_line, end_line FROM symbols
                 WHERE name = ?1 COLLATE NOCASE"
            )?;
            let rows = stmt.query_map(params![name], |r| Ok(Symbol {
                id:         r.get(0)?,
                path:       r.get(1)?,
                name:       r.get(2)?,
                kind:       r.get(3)?,
                start_line: r.get::<_, i64>(4)? as usize,
                end_line:   r.get::<_, i64>(5)? as usize,
            }))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(symbols)
    }

    /// All symbols defined in a path (for context.bundle).
    pub async fn symbols_in_file(&self, path: &str) -> Result<Vec<Symbol>> {
        let path    = path.to_string();
        let db_path = self.db_path.clone();

        let symbols = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Symbol>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT id, path, name, kind, start_line, end_line FROM symbols
                 WHERE path = ?1 ORDER BY start_line"
            )?;
            let rows = stmt.query_map(params![path], |r| Ok(Symbol {
                id:         r.get(0)?,
                path:       r.get(1)?,
                name:       r.get(2)?,
                kind:       r.get(3)?,
                start_line: r.get::<_, i64>(4)? as usize,
                end_line:   r.get::<_, i64>(5)? as usize,
            }))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(symbols)
    }

    /// Symbols called BY the given symbol id (outgoing calls).
    pub async fn callees(&self, symbol_id: &str) -> Result<Vec<CallRef>> {
        let sid     = symbol_id.to_string();
        let db_path = self.db_path.clone();

        let calls = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<CallRef>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT caller_id, callee_name, call_line FROM symbol_calls WHERE caller_id = ?1"
            )?;
            let rows = stmt.query_map(params![sid], |r| Ok(CallRef {
                caller_id:   r.get(0)?,
                callee_name: r.get(1)?,
                call_line:   r.get::<_, i64>(2)? as usize,
            }))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(calls)
    }

    /// Symbols that call the given symbol name (incoming calls).
    pub async fn callers(&self, symbol_name: &str) -> Result<Vec<CallRef>> {
        let name    = symbol_name.to_string();
        let db_path = self.db_path.clone();

        let calls = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<CallRef>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT caller_id, callee_name, call_line FROM symbol_calls WHERE callee_name = ?1"
            )?;
            let rows = stmt.query_map(params![name], |r| Ok(CallRef {
                caller_id:   r.get(0)?,
                callee_name: r.get(1)?,
                call_line:   r.get::<_, i64>(2)? as usize,
            }))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(calls)
    }

    /// Aggregate symbol-graph statistics for `status`. `top_n` caps the
    /// hot-symbols and largest-files lists.
    pub async fn graph_stats(&self, top_n: usize) -> Result<GraphStats> {
        let db_path = self.db_path.clone();
        let top_n   = top_n.max(1);

        let stats = tokio::task::spawn_blocking(move || -> anyhow::Result<GraphStats> {
            let conn = open_conn(&db_path)?;

            let count = |sql: &str| -> rusqlite::Result<usize> {
                conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|n| n as usize)
            };
            let total_symbols = count("SELECT COUNT(*) FROM symbols")?;
            let total_calls   = count("SELECT COUNT(*) FROM symbol_calls")?;
            let total_imports = count("SELECT COUNT(*) FROM symbol_imports")?;

            let pairs = |sql: &str, limit: i64| -> rusqlite::Result<Vec<(String, usize)>> {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
                })?;
                rows.collect()
            };

            let by_kind = {
                let mut stmt = conn.prepare(
                    "SELECT kind, COUNT(*) c FROM symbols GROUP BY kind ORDER BY c DESC"
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            // Only count calls that resolve to a project-defined symbol, so
            // std/library calls (unwrap, clone, …) don't drown the hotspots.
            let hot_symbols = pairs(
                "SELECT callee_name, COUNT(*) c FROM symbol_calls
                 WHERE callee_name IN (SELECT name FROM symbols)
                 GROUP BY callee_name ORDER BY c DESC LIMIT ?1",
                top_n as i64,
            )?;

            let top_files = pairs(
                "SELECT path, COUNT(*) c FROM symbols
                 GROUP BY path ORDER BY c DESC LIMIT ?1",
                top_n as i64,
            )?;

            Ok(GraphStats {
                total_symbols, total_calls, total_imports,
                by_kind, hot_symbols, top_files,
            })
        }).await??;

        Ok(stats)
    }

    /// Files that import from `module_path`.
    pub async fn importers_of(&self, module_path: &str) -> Result<Vec<String>> {
        let mp      = module_path.to_string();
        let db_path = self.db_path.clone();

        let paths = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT DISTINCT importer FROM symbol_imports WHERE from_module LIKE ?1"
            )?;
            let rows = stmt.query_map(params![format!("%{}%", mp)], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(paths)
    }
}

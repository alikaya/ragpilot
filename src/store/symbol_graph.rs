use anyhow::Result;
use rusqlite::params;
use std::path::PathBuf;

use crate::parser::{CallRef, Import, Symbol};
use super::sqlite::open_conn;

pub struct SymbolGraphStore {
    db_path: PathBuf,
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

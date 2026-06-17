use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;
use std::path::PathBuf;

use super::sqlite::open_conn;

pub struct ImpactIndexStore {
    db_path: PathBuf,
}

impl ImpactIndexStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    /// Record that `importer_path` imports from the given list of paths/modules.
    pub async fn update_imports(
        &self,
        importer_path: &str,
        imported_paths: &[String],
    ) -> Result<()> {
        let importer = importer_path.to_string();
        let imported = imported_paths.to_vec();
        let db_path  = self.db_path.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;
            conn.execute("DELETE FROM dependents WHERE dependent_path = ?1", params![importer])?;
            for imp in &imported {
                conn.execute(
                    "INSERT OR IGNORE INTO dependents (imported_path, dependent_path) VALUES (?1, ?2)",
                    params![imp, importer],
                )?;
            }
            Ok(())
        }).await??;

        Ok(())
    }

    /// Given a set of changed paths/symbols, return all files that directly
    /// or indirectly import from those paths (1 hop).
    pub async fn get_affected(&self, changed_paths: &[String]) -> Result<Vec<String>> {
        let paths   = changed_paths.to_vec();
        let db_path = self.db_path.clone();

        let affected = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT DISTINCT dependent_path FROM dependents WHERE imported_path LIKE ?1"
            )?;

            let mut results: HashSet<String> = HashSet::new();
            for path in &paths {
                let pattern = format!("%{}%", path);
                let rows = stmt.query_map(params![pattern], |r| r.get::<_, String>(0))?;
                for row in rows {
                    results.insert(row?);
                }
            }
            Ok(results.into_iter().collect())
        }).await??;

        Ok(affected)
    }

    /// Transitive BFS: find all affected files up to `max_hops`.
    pub async fn get_affected_transitive(
        &self,
        changed_paths: &[String],
        max_hops: usize,
    ) -> Result<Vec<String>> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut frontier: Vec<String> = changed_paths.to_vec();

        for _ in 0..max_hops {
            if frontier.is_empty() {
                break;
            }
            let next = self.get_affected(&frontier).await?;
            let new: Vec<String> = next.into_iter()
                .filter(|p| !visited.contains(p) && !frontier.contains(p))
                .collect();
            for p in &frontier {
                visited.insert(p.clone());
            }
            frontier = new;
        }
        for p in &frontier {
            visited.insert(p.clone());
        }

        // Remove the original changed paths from results
        for p in changed_paths {
            visited.remove(p);
        }

        Ok(visited.into_iter().collect())
    }
}

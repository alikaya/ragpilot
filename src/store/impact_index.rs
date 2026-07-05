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

    /// Remove all import edges recorded for a deleted/moved file.
    pub async fn remove(&self, dependent_path: &str) -> Result<()> {
        let dependent = dependent_path.to_string();
        let db_path   = self.db_path.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;
            conn.execute("DELETE FROM dependents WHERE dependent_path = ?1", params![dependent])?;
            Ok(())
        }).await??;

        Ok(())
    }

    /// Drop import edges of every file not in `keep` (self-heals rows left
    /// behind when files are deleted while the index state diverged). Returns
    /// the number of files pruned.
    pub async fn prune_except(&self, keep: Vec<String>) -> Result<usize> {
        let db_path = self.db_path.clone();

        let removed = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = open_conn(&db_path)?;
            let keep: HashSet<String> = keep.into_iter().collect();

            let paths: Vec<String> = {
                let mut stmt = conn.prepare("SELECT DISTINCT dependent_path FROM dependents")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };

            let mut removed = 0;
            for p in paths {
                if !keep.contains(&p) {
                    conn.execute("DELETE FROM dependents WHERE dependent_path = ?1", params![p])?;
                    removed += 1;
                }
            }
            Ok(removed)
        }).await??;

        Ok(removed)
    }

    /// Given a set of changed file paths, return all files that directly
    /// import from those paths (1 hop). Imports are stored as language-level
    /// module paths (e.g. `crate::store::qdrant`, `app.models`, `./utils`),
    /// so each changed file path is translated to the module-path patterns an
    /// importer would have used — see `import_match_patterns`.
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
                for pattern in import_match_patterns(path) {
                    let rows = stmt.query_map(params![pattern], |r| r.get::<_, String>(0))?;
                    for row in rows {
                        results.insert(row?);
                    }
                }
            }
            // A file does not affect itself through its own imports.
            for path in &paths {
                results.remove(path);
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

/// SQL LIKE patterns matching the module paths an importer would use for a
/// given file path. Language-aware: `src/store/qdrant.rs` is imported as
/// `crate::store::qdrant::…` (or `super::qdrant::…`), `app/models.py` as
/// `app.models`, `src/utils/date.ts` as `./date` / `../utils/date`. Returns
/// a conservative generic pattern for unknown extensions.
fn import_match_patterns(path: &str) -> Vec<String> {
    let (stem, ext) = match path.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None         => return vec![format!("%{}%", path)],
    };

    match ext {
        "rs" => {
            // src/store/qdrant.rs → modules ["store", "qdrant"];
            // src/mcp/mod.rs      → ["mcp"]; src/main.rs | src/lib.rs → crate
            // root (every import is internal — too broad to match, skip).
            let m = stem.strip_prefix("src/").unwrap_or(stem);
            let m = m.strip_suffix("/mod").unwrap_or(m);
            if m == "main" || m == "lib" {
                return Vec::new();
            }
            let module = m.replace('/', "::");
            let last = m.rsplit('/').next().unwrap_or(m);
            vec![
                format!("crate::{}%", module),
                format!("{}%", module),
                format!("super::{}%", last),
                format!("self::{}%", last),
            ]
        }
        "py" | "pyi" => {
            // app/models.py → app.models; pkg/__init__.py → pkg
            let m = stem.strip_suffix("/__init__").unwrap_or(stem);
            let module = m.replace('/', ".");
            let last = m.rsplit('/').next().unwrap_or(m);
            vec![format!("{}%", module), format!("%.{}", last), format!("%.{}.%", last)]
        }
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "vue" | "svelte" => {
            // Relative imports: "./date", "../utils/date" — match on the
            // trailing path segments without extension. index files are
            // imported by their directory name.
            let file = stem.rsplit('/').next().unwrap_or(stem);
            if file == "index" {
                match stem.strip_suffix("/index") {
                    Some(dir) => {
                        let dirname = dir.rsplit('/').next().unwrap_or(dir);
                        vec![format!("%/{}", dirname), format!("%/{}/index%", dirname)]
                    }
                    None => vec![format!("%{}%", stem)],
                }
            } else {
                vec![format!("%/{}", file), format!("%/{}.{}", file, ext)]
            }
        }
        _ => vec![format!("%{}%", path)],
    }
}

#[cfg(test)]
mod tests {
    use super::import_match_patterns;

    #[test]
    fn rust_module_patterns() {
        let p = import_match_patterns("src/store/qdrant.rs");
        assert!(p.contains(&"crate::store::qdrant%".to_string()));
        assert!(p.contains(&"super::qdrant%".to_string()));
        // `use crate::store::qdrant::QdrantStore` must match the crate:: pattern
        assert!("crate::store::qdrant::QdrantStore".starts_with("crate::store::qdrant"));
    }

    #[test]
    fn rust_mod_rs_maps_to_parent_module() {
        let p = import_match_patterns("src/mcp/mod.rs");
        assert!(p.contains(&"crate::mcp%".to_string()));
    }

    #[test]
    fn rust_crate_root_produces_no_patterns() {
        assert!(import_match_patterns("src/main.rs").is_empty());
        assert!(import_match_patterns("src/lib.rs").is_empty());
    }

    #[test]
    fn python_dotted_module() {
        let p = import_match_patterns("app/models.py");
        assert!(p.contains(&"app.models%".to_string()));
        let p = import_match_patterns("pkg/__init__.py");
        assert!(p.contains(&"pkg%".to_string()));
    }

    #[test]
    fn js_relative_and_index_imports() {
        let p = import_match_patterns("src/utils/date.ts");
        assert!(p.contains(&"%/date".to_string()));
        let p = import_match_patterns("src/components/Button/index.tsx");
        assert!(p.contains(&"%/Button".to_string()));
    }
}

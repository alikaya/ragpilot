use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::sqlite::open_conn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub path:       String,
    pub parent:     Option<String>,
    pub node_type:  String,   // "file" | "dir"
    pub language:   String,
    pub size_bytes: u64,
    pub hash:       String,
    pub depth:      usize,
}

pub struct ProjectTreeStore {
    db_path: PathBuf,
}

impl ProjectTreeStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub async fn upsert(&self, node: TreeNode) -> Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;
            conn.execute(
                "INSERT OR REPLACE INTO tree_nodes
                 (path, parent, node_type, language, size_bytes, hash, depth, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
                params![
                    node.path, node.parent, node.node_type, node.language,
                    node.size_bytes as i64, node.hash, node.depth as i64
                ],
            )?;
            Ok(())
        }).await??;
        Ok(())
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        let path    = path.to_string();
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = open_conn(&db_path)?;
            conn.execute("DELETE FROM tree_nodes WHERE path = ?1", params![path])?;
            Ok(())
        }).await??;
        Ok(())
    }

    /// Get all nodes whose path starts with `prefix`, up to `max_depth` deep.
    pub async fn get_subtree(
        &self,
        prefix: &str,
        max_depth: Option<usize>,
    ) -> Result<Vec<TreeNode>> {
        let prefix    = prefix.to_string();
        let max_depth = max_depth.unwrap_or(99) as i64;
        let db_path   = self.db_path.clone();

        let nodes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<TreeNode>> {
            let conn = open_conn(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT path, parent, node_type, language, size_bytes, hash, depth
                 FROM tree_nodes
                 WHERE path LIKE ?1 AND depth <= ?2
                 ORDER BY path"
            )?;
            let rows = stmt.query_map(params![format!("{}%", prefix), max_depth], |r| {
                Ok(TreeNode {
                    path:       r.get(0)?,
                    parent:     r.get(1)?,
                    node_type:  r.get(2)?,
                    language:   r.get(3)?,
                    size_bytes: r.get::<_, i64>(4)? as u64,
                    hash:       r.get(5)?,
                    depth:      r.get::<_, i64>(6)? as usize,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await??;

        Ok(nodes)
    }

    /// Compact listing: just paths within depth limit.
    pub async fn paths_in_dir(&self, dir: &str, max_depth: usize) -> Result<Vec<String>> {
        let nodes = self.get_subtree(dir, Some(max_depth)).await?;
        Ok(nodes.into_iter().map(|n| n.path).collect())
    }
}

/// Build a TreeNode from a filesystem path relative to root.
pub fn node_from_path(abs_path: &Path, root: &Path, hash: &str) -> TreeNode {
    let rel = abs_path.strip_prefix(root).unwrap_or(abs_path);
    let path_str = rel.to_string_lossy().to_string();

    let parent = rel.parent()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty());

    let ext = abs_path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let language = crate::indexer::file_language(&ext).to_string();
    let size_bytes = std::fs::metadata(abs_path).map(|m| m.len()).unwrap_or(0);
    let depth = rel.components().count().saturating_sub(1);

    TreeNode {
        path: path_str,
        parent,
        node_type: "file".into(),
        language,
        size_bytes,
        hash: hash.to_string(),
        depth,
    }
}

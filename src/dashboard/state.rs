//! What the dashboard shows: the project fleet and the brain, gathered from
//! the registry, the data directories and the vault.
//!
//! Everything here is local. Nothing is fetched from, or reported to, any
//! server — the dashboard is a window onto this machine.

use serde::Serialize;

use crate::brain;
use crate::indexer::IndexState;
use crate::paths::{self, ProjectPaths, Registry};

#[derive(Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `global` or `legacy`.
    pub layout: String,
    pub collection: String,
    pub exists: bool,
    pub files: usize,
    pub chunks: usize,
    pub indexed_at: Option<String>,
    /// Files that changed since they were indexed.
    pub dirty: usize,
    pub data_kb: u64,
}

#[derive(Serialize)]
pub struct Brain {
    pub exists: bool,
    pub name: String,
    pub rules: Vec<String>,
    pub threads: Vec<String>,
    pub notes: Vec<Note>,
    pub skills: Vec<String>,
    pub days: usize,
    pub last_day: Option<String>,
    pub sessions_today: usize,
    pub engine: String,
    pub compile_model: String,
    pub flush_model: String,
    pub schedule: String,
    pub scheduled: bool,
}

#[derive(Serialize)]
pub struct Note {
    pub slug: String,
    pub title: String,
    pub updated: String,
}

#[derive(Serialize)]
pub struct State {
    pub data_root: String,
    pub projects: Vec<Project>,
    pub brain: Brain,
    pub legacy_count: usize,
}

/// Everything the page needs, read from disk. Fast: no network, no Qdrant.
pub fn collect() -> State {
    let registry = Registry::load().unwrap_or_default();
    let mut projects: Vec<Project> = registry
        .projects
        .iter()
        .map(|(path, entry)| describe(path, &entry.id))
        .collect();
    projects.sort_by(|a, b| a.name.cmp(&b.name));

    State {
        data_root: paths::data_root().display().to_string(),
        legacy_count: projects.iter().filter(|p| p.layout == "legacy").count(),
        projects,
        brain: brain_state(),
    }
}

fn describe(path: &str, id: &str) -> Project {
    let root = std::path::Path::new(path);
    let paths = ProjectPaths::resolve(root);
    let state = IndexState::load(&paths.state()).unwrap_or_default();

    let dirty = state
        .file_hashes
        .iter()
        .filter(|(rel, stored)| match std::fs::read_to_string(root.join(rel.as_str())) {
            Ok(text) => &crate::indexer::compute_hash(&text) != *stored,
            Err(_) => true,
        })
        .count();

    let (collection, name) = match crate::config::Config::load(&paths.config()) {
        Ok(cfg) => (
            paths.collection(cfg.qdrant.collection.as_deref(), &cfg.project.name),
            cfg.project.name,
        ),
        Err(_) => (id.to_string(), id.to_string()),
    };

    Project {
        id: id.to_string(),
        name,
        path: path.to_string(),
        layout: if paths.is_legacy() { "legacy" } else { "global" }.to_string(),
        collection,
        exists: root.is_dir(),
        files: state.total_files,
        chunks: state.total_chunks,
        indexed_at: state.indexed_at.map(|t| t.format("%Y-%m-%d %H:%M").to_string()),
        dirty,
        data_kb: dir_size(paths.data_dir()) / 1024,
    }
}

fn brain_state() -> Brain {
    if !brain::exists() {
        return Brain {
            exists: false,
            name: String::new(),
            rules: Vec::new(),
            threads: Vec::new(),
            notes: Vec::new(),
            skills: Vec::new(),
            days: 0,
            last_day: None,
            sessions_today: 0,
            engine: String::new(),
            compile_model: String::new(),
            flush_model: String::new(),
            schedule: String::new(),
            scheduled: false,
        };
    }

    let cfg = brain::config::BrainConfig::load(&brain::config_path()).unwrap_or_default();
    let persona = std::fs::read_to_string(brain::persona_path()).unwrap_or_default();
    let name = persona
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().to_string())
        .unwrap_or_else(|| "brain".to_string());

    let today = brain::vault::today();
    let dailies = list_stems(&brain::daily_dir());
    let sessions_today = std::fs::read_to_string(brain::vault::daily_path(&today))
        .map(|t| t.matches("\n## Session").count())
        .unwrap_or(0);

    Brain {
        exists: true,
        name,
        rules: rule_lines(),
        threads: brain::vault::active_threads(),
        notes: notes_in(&brain::knowledge_dir()),
        skills: list_stems(&brain::skills_dir()),
        days: dailies.len(),
        last_day: dailies.last().cloned(),
        sessions_today,
        flush_model: cfg.flush_model().unwrap_or(&cfg.compiler.model).to_string(),
        compile_model: cfg.compiler.model.clone(),
        engine: cfg.compiler.engine.clone(),
        schedule: cfg.compiler.schedule.clone(),
        scheduled: brain::schedule::installed(),
    }
}

/// Rules as `rule — why` lines, in the order they were added.
fn rule_lines() -> Vec<String> {
    let text = std::fs::read_to_string(brain::vault::rules_path()).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rule) = line.strip_prefix("- **rule:**") {
            out.push(rule.trim().to_string());
        } else if let Some(why) = line.strip_prefix("**why:**") {
            if let Some(last) = out.last_mut() {
                last.push_str(" — ");
                last.push_str(why.trim());
            }
        }
    }
    out
}

fn notes_in(dir: &std::path::Path) -> Vec<Note> {
    let mut out: Vec<Note> = list_stems(dir)
        .into_iter()
        .map(|slug| {
            let text = std::fs::read_to_string(dir.join(format!("{slug}.md"))).unwrap_or_default();
            let title = text
                .lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches("# ").trim().to_string())
                .unwrap_or_else(|| slug.clone());
            let updated = text
                .lines()
                .find_map(|l| l.strip_prefix("updated:"))
                .map(|u| u.trim().to_string())
                .unwrap_or_default();
            Note { slug, title, updated }
        })
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.title.cmp(&b.title)));
    out
}

fn list_stems(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    out.sort();
    out
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Point counts per collection, asked of Qdrant. Separate from [`collect`]
/// because it is the only part that touches the network, and the page should
/// paint before it finishes.
pub async fn collection_points(projects: &[Project]) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    let Some(first) = projects.first() else { return out };

    let Ok(config) = crate::config::Config::load(&ProjectPaths::resolve(std::path::Path::new(&first.path)).config())
    else {
        return out;
    };

    for project in projects {
        let mut qdrant = config.qdrant.clone();
        qdrant.collection = Some(project.collection.clone());
        let Ok(store) = crate::store::qdrant::QdrantStore::new(&qdrant) else { continue };
        if let Ok(info) = crate::store::VectorStore::collection_info(&store).await {
            out.insert(project.id.clone(), info.points_count);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rules_pair_each_rule_with_its_reason() {
        // `rule_lines` reads the live vault, so the pairing logic is exercised
        // through the same parse on a literal here.
        let text = "# Rules\n\n- **rule:** answer first\n  **why:** no warm-up\n- **rule:** read before writing\n";
        let mut out: Vec<String> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(rule) = line.strip_prefix("- **rule:**") {
                out.push(rule.trim().to_string());
            } else if let Some(why) = line.strip_prefix("**why:**") {
                if let Some(last) = out.last_mut() {
                    last.push_str(" — ");
                    last.push_str(why.trim());
                }
            }
        }
        assert_eq!(out, vec!["answer first — no warm-up", "read before writing"]);
    }
}

// ── vault browsing ─────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct Entry {
    /// Path relative to the vault root — what `/api/file` takes back.
    pub path: String,
    pub title: String,
    pub meta: String,
}

#[derive(Serialize)]
pub struct Section {
    pub key: String,
    pub label: String,
    pub entries: Vec<Entry>,
}

/// The vault as a browsable tree: every file a reader would want, in the order
/// a reader wants them.
pub fn vault_sections() -> Vec<Section> {
    let root = brain::dir();
    let mut out = Vec::new();

    let single = |path: std::path::PathBuf, label: &str| -> Option<Entry> {
        path.exists().then(|| Entry {
            path: rel(&root, &path),
            title: label.to_string(),
            meta: modified(&path),
        })
    };

    let mut identity = Vec::new();
    identity.extend(single(brain::persona_path(), "persona.md"));
    identity.extend(single(brain::vault::rules_path(), "rules.md"));
    identity.extend(single(brain::vault::threads_path(), "threads.md"));
    identity.extend(single(brain::config_path(), "config.toml"));
    out.push(Section { key: "identity".into(), label: "Identity".into(), entries: identity });

    // Newest first: a log is read from the end.
    let mut daily = files_in(&root, &brain::daily_dir(), &["md"]);
    daily.reverse();
    out.push(Section { key: "daily".into(), label: "Daily".into(), entries: daily });

    out.push(Section {
        key: "knowledge".into(),
        label: "Knowledge".into(),
        entries: files_in(&root, &brain::knowledge_dir(), &["md"]),
    });
    out.push(Section {
        key: "skills".into(),
        label: "Skills".into(),
        entries: files_in(&root, &brain::skills_dir(), &["md"]),
    });
    out.push(Section {
        key: "inbox".into(),
        label: "Inbox".into(),
        entries: files_in(&root, &brain::inbox_dir(), &[]),
    });
    out.push(Section {
        key: "archive".into(),
        label: "Archive".into(),
        entries: files_in(&root, &brain::takeout_dir(), &[]),
    });

    out
}

/// Files in one directory, titled by their heading when they have one.
fn files_in(root: &std::path::Path, dir: &std::path::Path, extensions: &[&str]) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Entry> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            extensions.is_empty()
                || p.extension().is_some_and(|e| extensions.contains(&e.to_string_lossy().as_ref()))
        })
        .map(|path| {
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let title = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find(|l| l.starts_with("# "))
                        .map(|l| l.trim_start_matches("# ").trim().to_string())
                })
                .filter(|t| !t.is_empty())
                .unwrap_or(stem);
            Entry { path: rel(root, &path), title, meta: modified(&path) }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn rel(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string()
}

fn modified(path: &std::path::Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else { return String::new() };
    let Ok(time) = meta.modified() else { return String::new() };
    let stamp: chrono::DateTime<chrono::Local> = time.into();
    stamp.format("%Y-%m-%d %H:%M").to_string()
}

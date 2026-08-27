//! Global data-directory resolution and the project registry (Phase 1).
//!
//! Everything RagPilot writes about a project lives under one machine-global
//! root — `~/.local/share/ragpilot/` by default — instead of a `.rag/` folder
//! inside the project. The project folder keeps only the MCP config and the
//! agent markdown file.
//!
//! ```text
//! <data_root>/
//!   config.toml        global defaults (qdrant url, embedding, chunking)
//!   registry.json      canonical project path → project id
//!   projects/<id>/     config.toml, state.json, stores.db, queries/
//!   brain/             the second-brain vault (Phase A)
//! ```
//!
//! A project id is `<slug(folder-name)>-<blake3(canonical path)[..8]>`: readable
//! at a glance, and collision-free because the hash covers the full path. It is
//! also the Qdrant collection name, so the id is restricted to `[a-z0-9-]`.
//!
//! This module only *resolves* paths and keeps the registry. Moving the actual
//! stores onto these paths is Phase 2; writing registry entries at `init` time
//! is Phase 3.

// The registry API is complete on purpose: `projects_dir`/`project_dir` are the
// write targets Phase 2 redirects the stores onto, `upsert`/`relink`/`remove`
// back the `projects` subcommands in Phase 4, and `brain_dir` is Phase A's
// vault. All of it is exercised by the tests below; the allow only silences
// "not called from `src/` yet".
#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Overrides the whole data root — set by tests, sandboxes and power users.
pub const DATA_DIR_ENV: &str = "RAGPILOT_DATA_DIR";

/// Bumped only on a breaking `registry.json` shape change.
pub const REGISTRY_VERSION: u32 = 1;

/// Longest slug part of a project id; the hash suffix keeps it unique anyway.
const MAX_SLUG_LEN: usize = 32;

// ── Directory layout ───────────────────────────────────────────────────────

/// Root of all RagPilot data: `$RAGPILOT_DATA_DIR`, else the XDG data dir
/// (`~/.local/share/ragpilot`), else `~/.local/ragpilot`.
pub fn data_root() -> PathBuf {
    resolve_data_root(std::env::var_os(DATA_DIR_ENV), dirs::data_dir(), dirs::home_dir())
}

/// Pure core of [`data_root`] — the inputs are injected so it can be tested
/// without mutating process-wide environment state.
fn resolve_data_root(
    env_override: Option<OsString>,
    xdg_data_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(raw) = env_override {
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    if let Some(d) = xdg_data_dir {
        return d.join("ragpilot");
    }
    home.unwrap_or_else(|| PathBuf::from(".")).join(".local").join("ragpilot")
}

/// Global defaults, the lowest-priority config layer (see `config::Config`).
pub fn global_config_path() -> PathBuf { data_root().join("config.toml") }

/// Canonical-path → project-id map.
pub fn registry_path() -> PathBuf { data_root().join("registry.json") }

/// Parent of every per-project data directory.
pub fn projects_dir() -> PathBuf { data_root().join("projects") }

/// Per-project data directory: `config.toml`, `state.json`, `stores.db`.
pub fn project_dir(id: &str) -> PathBuf { projects_dir().join(id) }

/// The second-brain vault (Phase A) — one per machine, not per project.
pub fn brain_dir() -> PathBuf { data_root().join("brain") }

// ── Identity ───────────────────────────────────────────────────────────────

/// Resolve symlinks and `..` so the same folder always yields the same key.
///
/// The path must exist — an id derived from a guessed path would not survive
/// the next lookup.
pub fn canonical(path: &Path) -> Result<PathBuf> {
    let resolved = path
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {}", path.display()))?;
    Ok(strip_verbatim(&resolved))
}

/// Drop Windows' `\\?\` / `\\?\UNC\` verbatim prefixes that `canonicalize`
/// adds, so registry keys match what a user types and what other tools print.
fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Lowercase ASCII slug usable as both a directory name and a Qdrant
/// collection name. Turkish letters are transliterated rather than dropped, so
/// `Çalışma-Alanı` stays readable as `calisma-alani`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in name.chars() {
        let mapped = match ch {
            'ç' | 'Ç' => Some("c"),
            'ğ' | 'Ğ' => Some("g"),
            'ı' | 'I' => Some("i"),
            'İ' | 'i' => Some("i"),
            'ö' | 'Ö' => Some("o"),
            'ş' | 'Ş' => Some("s"),
            'ü' | 'Ü' => Some("u"),
            _ => None,
        };

        match mapped {
            Some(m) => {
                if pending_dash && !out.is_empty() { out.push('-'); }
                pending_dash = false;
                out.push_str(m);
            }
            None if ch.is_ascii_alphanumeric() => {
                if pending_dash && !out.is_empty() { out.push('-'); }
                pending_dash = false;
                out.push(ch.to_ascii_lowercase());
            }
            None => pending_dash = true,
        }

        if out.len() >= MAX_SLUG_LEN {
            break;
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() { "project".to_string() } else { trimmed.to_string() }
}

/// `<slug(folder-name)>-<blake3(path)[..8]>`.
///
/// Two folders with the same name in different places get different ids
/// because the hash covers the full canonical path.
pub fn project_id(canonical_path: &Path) -> String {
    let name = canonical_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let digest = blake3::hash(canonical_path.to_string_lossy().as_bytes()).to_hex();
    format!("{}-{}", slug(&name), &digest[..8])
}

// ── Registry ───────────────────────────────────────────────────────────────

/// One registered project. Keyed by canonical path in [`Registry::projects`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub id: String,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<String>,
}

/// `registry.json` — the canonical-path → project-id map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    pub projects: BTreeMap<String, ProjectEntry>,
    /// Where this registry was read from; not serialized.
    #[serde(skip)]
    path: PathBuf,
}

impl Default for Registry {
    fn default() -> Self {
        Self { version: REGISTRY_VERSION, projects: BTreeMap::new(), path: registry_path() }
    }
}

impl Registry {
    /// Load the machine registry, or an empty one when it does not exist yet.
    pub fn load() -> Result<Self> {
        Self::load_from(&registry_path())
    }

    /// Load from an explicit path — used by tests and by `migrate`.
    ///
    /// A missing file is not an error (nothing registered yet); a corrupt one
    /// is, because silently starting from empty would orphan every project.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self { path: path.to_path_buf(), ..Default::default() });
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read registry: {}", path.display()))?;
        let mut reg: Self = serde_json::from_str(&raw)
            .with_context(|| format!("Cannot parse registry: {}", path.display()))?;
        reg.path = path.to_path_buf();
        Ok(reg)
    }

    /// Where this registry lives on disk.
    pub fn path(&self) -> &Path { &self.path }

    /// Atomic write: a sibling temp file, then a rename. A crash mid-write
    /// leaves the previous registry intact rather than a truncated one.
    pub fn save(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Registry path has no parent: {}", self.path.display()))?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Cannot create data dir: {}", dir.display()))?;

        let body = serde_json::to_string_pretty(self)?;
        let tmp = dir.join(format!(".registry.json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, body)
            .with_context(|| format!("Cannot write registry temp file: {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("Cannot commit registry: {}", self.path.display()))?;
        Ok(())
    }

    /// Look a project up by its canonical path.
    pub fn lookup(&self, canonical_path: &Path) -> Option<&ProjectEntry> {
        self.projects.get(&key(canonical_path))
    }

    /// Look a project up by id, returning its registered path too.
    pub fn lookup_by_id(&self, id: &str) -> Option<(&str, &ProjectEntry)> {
        self.projects
            .iter()
            .find(|(_, e)| e.id == id)
            .map(|(p, e)| (p.as_str(), e))
    }

    /// Register a path, or return the existing entry unchanged. Idempotent:
    /// re-running `init` never rewrites `created` or invalidates the id.
    pub fn upsert(&mut self, canonical_path: &Path) -> &ProjectEntry {
        let k = key(canonical_path);
        self.projects.entry(k).or_insert_with(|| ProjectEntry {
            id: project_id(canonical_path),
            created: now(),
            last_indexed: None,
        })
    }

    /// Stamp the last successful index. No-op for an unregistered path.
    pub fn touch_indexed(&mut self, canonical_path: &Path) {
        if let Some(entry) = self.projects.get_mut(&key(canonical_path)) {
            entry.last_indexed = Some(now());
        }
    }

    /// Forget a path. Returns the removed entry, if there was one.
    /// Deleting the project's data directory is the caller's job.
    pub fn remove(&mut self, canonical_path: &Path) -> Option<ProjectEntry> {
        self.projects.remove(&key(canonical_path))
    }

    /// Forget a project by id.
    pub fn remove_id(&mut self, id: &str) -> Option<ProjectEntry> {
        let path = self.lookup_by_id(id).map(|(p, _)| p.to_string())?;
        self.projects.remove(&path)
    }

    /// Point an existing id at a new path after the folder was moved.
    ///
    /// The id is deliberately kept as-is even though it no longer matches
    /// `project_id(new_path)`: the id names the data directory and the Qdrant
    /// collection, so changing it would strand the existing index.
    pub fn relink(&mut self, id: &str, new_canonical_path: &Path) -> Result<()> {
        let old = self
            .lookup_by_id(id)
            .map(|(p, _)| p.to_string())
            .ok_or_else(|| anyhow::anyhow!("No project registered with id '{id}'"))?;

        let new_key = key(new_canonical_path);
        if let Some(existing) = self.projects.get(&new_key) {
            if existing.id != id {
                anyhow::bail!(
                    "{} is already registered as '{}'",
                    new_canonical_path.display(),
                    existing.id
                );
            }
            return Ok(());
        }

        let entry = self.projects.remove(&old).expect("looked up above");
        self.projects.insert(new_key, entry);
        Ok(())
    }

    /// Registered projects whose folder name matches `canonical_path` but whose
    /// registered path no longer exists — i.e. likely the same project, moved.
    /// Drives the `relink` hint; never acts on its own.
    pub fn moved_candidates(&self, canonical_path: &Path) -> Vec<(String, String)> {
        let target_slug = canonical_path
            .file_name()
            .map(|n| slug(&n.to_string_lossy()))
            .unwrap_or_default();

        self.projects
            .iter()
            .filter(|(path, _)| !Path::new(path).exists())
            .filter(|(path, _)| {
                Path::new(path)
                    .file_name()
                    .map(|n| slug(&n.to_string_lossy()) == target_slug)
                    .unwrap_or(false)
            })
            .map(|(path, entry)| (entry.id.clone(), path.clone()))
            .collect()
    }
}

/// Registry keys are the canonical path as a string.
fn key(canonical_path: &Path) -> String {
    canonical_path.to_string_lossy().to_string()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ── Project resolution ─────────────────────────────────────────────────────

/// What a folder resolves to when the MCP server (or a CLI command) starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// In the registry — the normal case after `ragpilot init`.
    Registered { id: String, root: PathBuf },
    /// Not registered but has a `.rag/` directory: a pre-global-layout
    /// project. Supported for one minor release, with a migrate nudge.
    Legacy { root: PathBuf },
    /// Not registered, and a registered project with the same folder name has
    /// a path that no longer exists — the folder was probably moved.
    Moved { root: PathBuf, candidates: Vec<(String, String)> },
    /// Unknown folder: `ragpilot init` has not been run here.
    Unregistered { root: PathBuf },
}

impl Resolution {
    /// The canonical project root, whichever way it resolved.
    pub fn root(&self) -> &Path {
        match self {
            Self::Registered { root, .. }
            | Self::Legacy { root }
            | Self::Moved { root, .. }
            | Self::Unregistered { root } => root,
        }
    }

    /// True when tools can serve this folder today (Phase 1 keeps legacy
    /// `.rag/` projects working exactly as before).
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Registered { .. } | Self::Legacy { .. })
    }

    /// One-paragraph, actionable explanation — shown on stderr at startup and
    /// returned to the agent when a tool is called with no project loaded.
    pub fn message(&self) -> String {
        match self {
            Self::Registered { id, root } => {
                format!("ragpilot: project '{id}' ({})", root.display())
            }
            Self::Legacy { root } => format!(
                "ragpilot: {} still uses a project-local .rag/ directory. It keeps working \
                 for now — run `ragpilot migrate` to move it under {}.",
                root.display(),
                data_root().display()
            ),
            Self::Moved { root, candidates } => {
                let list = candidates
                    .iter()
                    .map(|(id, old)| format!("'{id}' (was {old})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "ragpilot: {} is not registered, but a registered project looks like it \
                     moved here: {list}. Run `ragpilot projects relink <id> {}` — or \
                     `ragpilot init .` to register it as a new project.",
                    root.display(),
                    root.display()
                )
            }
            Self::Unregistered { root } => format!(
                "ragpilot: {} is not a registered project — run `ragpilot init .` there first. \
                 For folder-independent clients, pass `--root <path>` or set RAGPILOT_ROOT.",
                root.display()
            ),
        }
    }
}

/// Resolve a folder against the registry. Precedence: registered → legacy
/// `.rag/` → moved-folder guess → unregistered.
pub fn resolve_project(registry: &Registry, path: &Path) -> Resolution {
    let root = canonical(path).unwrap_or_else(|_| path.to_path_buf());

    if let Some(entry) = registry.lookup(&root) {
        return Resolution::Registered { id: entry.id.clone(), root };
    }
    if legacy_dir(&root).join("config.toml").exists() {
        return Resolution::Legacy { root };
    }
    let candidates = registry.moved_candidates(&root);
    if !candidates.is_empty() {
        return Resolution::Moved { root, candidates };
    }
    Resolution::Unregistered { root }
}

// ── Per-project paths ──────────────────────────────────────────────────────

/// Where one project's data actually lives, and what its collection is called.
///
/// Two layouts coexist during the transition:
/// * **global** — `<data_root>/projects/<id>/`, the target layout;
/// * **legacy** — the project's own `.rag/`, for folders that predate it.
///
/// Resolution is registry-first, so a registered project never falls back to a
/// stale `.rag/` that happens to still be lying around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    root: PathBuf,
    data: PathBuf,
    /// `None` for the legacy layout — a `.rag/` project has no id.
    id: Option<String>,
}

impl ProjectPaths {
    /// Registry first, then a legacy `.rag/`, then the global slot this folder
    /// *would* get — which is what `init` is about to create.
    pub fn resolve(root: &Path) -> Self {
        let registry = load_registry_lenient();
        Self::resolve_with(&registry, root)
    }

    /// [`resolve`](Self::resolve) against an already-loaded registry — avoids
    /// re-reading `registry.json` when the caller already has it.
    pub fn resolve_with(registry: &Registry, root: &Path) -> Self {
        let root = canonical(root).unwrap_or_else(|_| root.to_path_buf());

        if let Some(entry) = registry.lookup(&root) {
            let id = entry.id.clone();
            return Self { data: project_dir(&id), root, id: Some(id) };
        }
        if legacy_dir(&root).join("config.toml").exists() {
            return Self { data: legacy_dir(&root), root, id: None };
        }
        Self::global(&root)
    }

    /// The global slot for a folder, whether or not it is registered yet.
    /// `init` and `migrate` write here; resolution never downgrades to `.rag/`.
    pub fn global(root: &Path) -> Self {
        let root = canonical(root).unwrap_or_else(|_| root.to_path_buf());
        let id = project_id(&root);
        Self { data: project_dir(&id), root, id: Some(id) }
    }

    /// The project's own `.rag/` — the migrate *source*, never a write target
    /// for new projects.
    pub fn legacy(root: &Path) -> Self {
        let root = canonical(root).unwrap_or_else(|_| root.to_path_buf());
        Self { data: legacy_dir(&root), root, id: None }
    }

    pub fn root(&self) -> &Path { &self.root }
    pub fn data_dir(&self) -> &Path { &self.data }
    pub fn id(&self) -> Option<&str> { self.id.as_deref() }
    pub fn is_legacy(&self) -> bool { self.id.is_none() }

    pub fn config(&self) -> PathBuf { self.data.join("config.toml") }
    pub fn state(&self) -> PathBuf { self.data.join("state.json") }
    pub fn stores_db(&self) -> PathBuf { self.data.join("stores.db") }
    pub fn lock(&self) -> PathBuf { self.data.join("index.lock") }

    /// Tree-sitter query overrides. A project-local `.rag/queries/` still wins
    /// when it exists — it is read-only, hand-authored and worth keeping next
    /// to the code it describes; otherwise the global slot is used.
    pub fn queries(&self) -> PathBuf {
        let project_local = legacy_dir(&self.root).join("queries");
        if project_local.is_dir() {
            return project_local;
        }
        self.data.join("queries")
    }

    /// Qdrant collection name: an explicit `[qdrant] collection` wins, then the
    /// project id (global layout), then the legacy lowercased project name.
    pub fn collection(&self, explicit: Option<&str>, project_name: &str) -> String {
        match (explicit, &self.id) {
            (Some(name), _) => normalize_collection(name),
            (None, Some(id)) => id.clone(),
            (None, None) => normalize_collection(project_name),
        }
    }

    /// What this project's collection was called before the global layout —
    /// used to detect an un-migrated index instead of silently creating a
    /// second, empty collection beside it.
    pub fn legacy_collection(&self, project_name: &str) -> String {
        normalize_collection(project_name)
    }
}

/// The pre-global, project-local data directory.
pub fn legacy_dir(root: &Path) -> PathBuf { root.join(".rag") }

/// Qdrant collection names: lowercase, no spaces or dashes.
pub fn normalize_collection(raw: &str) -> String {
    raw.to_lowercase().replace([' ', '-'], "_")
}

/// Load the registry for path resolution. A broken registry must not silently
/// route a registered project to a fresh, empty data directory, so it is
/// reported — once per process — before falling back to empty.
fn load_registry_lenient() -> Registry {
    match Registry::load() {
        Ok(reg) => reg,
        Err(e) => {
            static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            if WARNED.set(()).is_ok() {
                eprintln!(
                    "ragpilot: {} is unreadable ({e}) — treating every project as unregistered. \
                     Fix or delete it before indexing.",
                    registry_path().display()
                );
            }
            Registry::default()
        }
    }
}

/// Canonicalize a folder, record it in the registry and create its data
/// directory. Idempotent — re-running `init` neither changes the id nor
/// rewrites `created`.
///
/// A legacy `.rag/` project is left alone: adopting it into the registry is
/// `ragpilot migrate`'s job, not something `init` should do behind the user's
/// back while its data still lives in the project folder.
pub fn register_project(root: &Path) -> Result<ProjectPaths> {
    let paths = ProjectPaths::resolve(root);
    if paths.is_legacy() {
        return Ok(paths);
    }

    let mut registry = Registry::load()?;
    registry.upsert(paths.root());
    registry.save()?;

    std::fs::create_dir_all(paths.data_dir()).with_context(|| {
        format!("Cannot create project data dir: {}", paths.data_dir().display())
    })?;
    Ok(paths)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique scratch directory. Avoids a `tempfile` dependency and stays
    /// unique across parallel test threads.
    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-paths-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn data_root_prefers_env_then_xdg_then_home() {
        assert_eq!(
            resolve_data_root(Some("/custom/root".into()), Some("/xdg".into()), Some("/home/u".into())),
            PathBuf::from("/custom/root")
        );
        // An empty override is treated as unset, not as the filesystem root.
        assert_eq!(
            resolve_data_root(Some("".into()), Some("/xdg".into()), Some("/home/u".into())),
            PathBuf::from("/xdg/ragpilot")
        );
        assert_eq!(
            resolve_data_root(None, Some("/xdg".into()), Some("/home/u".into())),
            PathBuf::from("/xdg/ragpilot")
        );
        assert_eq!(
            resolve_data_root(None, None, Some("/home/u".into())),
            PathBuf::from("/home/u/.local/ragpilot")
        );
    }

    #[test]
    fn slug_is_collection_safe() {
        assert_eq!(slug("ragpilot"), "ragpilot");
        assert_eq!(slug("My Project"), "my-project");
        assert_eq!(slug("rag_cli.v2"), "rag-cli-v2");
        assert_eq!(slug("--edge--"), "edge");
        assert_eq!(slug("Çalışma Alanı"), "calisma-alani");
        assert_eq!(slug("日本語"), "project");
        assert!(slug("a-very-long-folder-name-that-keeps-going-and-going").len() <= MAX_SLUG_LEN);
        assert!(slug("Weird!!Name??").chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn project_id_is_stable_and_path_scoped() {
        let a = Path::new("/home/u/dev/ragpilot");
        let b = Path::new("/home/u/other/ragpilot");

        // Same folder, every call: same id.
        assert_eq!(project_id(a), project_id(a));
        // Same folder name, different place: different id.
        assert_ne!(project_id(a), project_id(b));
        // Readable prefix, 8-char hash suffix.
        assert!(project_id(a).starts_with("ragpilot-"));
        assert_eq!(project_id(a).len(), "ragpilot-".len() + 8);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_real_path_resolve_to_one_project() {
        let base = scratch("symlink");
        let real = base.join("real-project");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link-to-project");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let via_real = canonical(&real).unwrap();
        let via_link = canonical(&link).unwrap();
        assert_eq!(via_real, via_link);
        assert_eq!(project_id(&via_real), project_id(&via_link));

        // And via a `..` detour.
        let detour = real.join("..").join("real-project");
        assert_eq!(canonical(&detour).unwrap(), via_real);

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn strip_verbatim_normalizes_windows_prefixes() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\dev\ragpilot")),
            PathBuf::from(r"C:\dev\ragpilot")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\ragpilot")),
            PathBuf::from(r"\\server\share\ragpilot")
        );
        // Plain paths are untouched on every platform.
        assert_eq!(strip_verbatim(Path::new("/home/u/dev")), PathBuf::from("/home/u/dev"));
    }

    #[test]
    fn registry_round_trips_atomically() {
        let dir = scratch("registry");
        let path = dir.join("registry.json");
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let project = canonical(&project).unwrap();

        let mut reg = Registry::load_from(&path).unwrap();
        assert!(reg.projects.is_empty());

        let id = reg.upsert(&project).id.clone();
        reg.touch_indexed(&project);
        reg.save().unwrap();

        let reloaded = Registry::load_from(&path).unwrap();
        assert_eq!(reloaded.version, REGISTRY_VERSION);
        assert_eq!(reloaded.lookup(&project).map(|e| e.id.as_str()), Some(id.as_str()));
        assert!(reloaded.lookup(&project).unwrap().last_indexed.is_some());
        assert_eq!(reloaded.lookup_by_id(&id).map(|(p, _)| p), Some(key(&project).as_str()));

        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "atomic write left a temp file behind");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn upsert_is_idempotent() {
        let dir = scratch("idempotent");
        let project = canonical(&dir).unwrap();
        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();

        let first = reg.upsert(&project).clone();
        reg.touch_indexed(&project);
        let second = reg.upsert(&project).clone();

        assert_eq!(first.id, second.id);
        assert_eq!(first.created, second.created);
        assert_eq!(reg.projects.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relink_keeps_the_id_and_rejects_collisions() {
        let dir = scratch("relink");
        let old = dir.join("old-home");
        let new = dir.join("new-home");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let (old, new) = (canonical(&old).unwrap(), canonical(&new).unwrap());

        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        let id = reg.upsert(&old).id.clone();

        reg.relink(&id, &new).unwrap();
        assert!(reg.lookup(&old).is_none());
        assert_eq!(reg.lookup(&new).map(|e| e.id.as_str()), Some(id.as_str()));

        // Relinking onto a path owned by another project is refused.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let other = canonical(&other).unwrap();
        let other_id = reg.upsert(&other).id.clone();
        assert!(reg.relink(&id, &other).is_err());
        assert_eq!(reg.lookup(&other).map(|e| e.id.as_str()), Some(other_id.as_str()));

        assert!(reg.relink("no-such-id", &new).is_err());

        // Removal by id clears the entry.
        assert!(reg.remove_id(&id).is_some());
        assert!(reg.lookup(&new).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolution_precedence() {
        let dir = scratch("resolve");
        let registered = dir.join("registered");
        let legacy = dir.join("legacy");
        let unknown = dir.join("unknown");
        for p in [&registered, &legacy, &unknown] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::create_dir_all(legacy.join(".rag")).unwrap();
        std::fs::write(legacy.join(".rag").join("config.toml"), "").unwrap();

        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        let id = reg.upsert(&canonical(&registered).unwrap()).id.clone();

        match resolve_project(&reg, &registered) {
            Resolution::Registered { id: got, .. } => assert_eq!(got, id),
            other => panic!("expected Registered, got {other:?}"),
        }
        assert!(matches!(resolve_project(&reg, &legacy), Resolution::Legacy { .. }));
        assert!(matches!(resolve_project(&reg, &unknown), Resolution::Unregistered { .. }));

        // A registered project whose folder vanished, re-appearing elsewhere
        // under the same name, is reported as moved — with a relink hint.
        let vanished = dir.join("gone").join("moved-app");
        std::fs::create_dir_all(&vanished).unwrap();
        let vanished_canon = canonical(&vanished).unwrap();
        let vanished_id = reg.upsert(&vanished_canon).id.clone();
        std::fs::remove_dir_all(dir.join("gone")).unwrap();

        let reappeared = dir.join("moved-app");
        std::fs::create_dir_all(&reappeared).unwrap();
        match resolve_project(&reg, &reappeared) {
            Resolution::Moved { candidates, .. } => {
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].0, vanished_id);
            }
            other => panic!("expected Moved, got {other:?}"),
        }
        assert!(resolve_project(&reg, &reappeared).message().contains("relink"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn usability_matches_phase_one_behaviour() {
        let root = PathBuf::from("/tmp/x");
        assert!(Resolution::Registered { id: "x-1".into(), root: root.clone() }.is_usable());
        assert!(Resolution::Legacy { root: root.clone() }.is_usable());
        assert!(!Resolution::Unregistered { root: root.clone() }.is_usable());
        assert!(!Resolution::Moved { root, candidates: vec![] }.is_usable());
    }

    #[test]
    fn corrupt_registry_is_an_error_not_an_empty_map() {
        let dir = scratch("corrupt");
        let path = dir.join("registry.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Registry::load_from(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod project_paths_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-pp-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registered_project_writes_nothing_into_the_project_folder() {
        let dir = scratch("global");
        let project = canonical(&dir).unwrap();

        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        let id = reg.upsert(&project).id.clone();
        let paths = ProjectPaths::resolve_with(&reg, &project);

        assert_eq!(paths.id(), Some(id.as_str()));
        assert!(!paths.is_legacy());
        for p in [paths.config(), paths.state(), paths.stores_db(), paths.lock(), paths.queries()] {
            assert!(
                !p.starts_with(&project),
                "{} would be written inside the project folder",
                p.display()
            );
            assert!(p.starts_with(project_dir(&id)), "{} escaped the project slot", p.display());
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_project_keeps_using_its_own_rag_dir() {
        let dir = scratch("legacy");
        std::fs::create_dir_all(dir.join(".rag")).unwrap();
        std::fs::write(dir.join(".rag").join("config.toml"), "").unwrap();
        let project = canonical(&dir).unwrap();

        let reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        let paths = ProjectPaths::resolve_with(&reg, &project);

        assert!(paths.is_legacy());
        assert_eq!(paths.config(), project.join(".rag").join("config.toml"));
        assert_eq!(paths.state(), project.join(".rag").join("state.json"));
        assert_eq!(paths.stores_db(), project.join(".rag").join("stores.db"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_wins_over_a_leftover_rag_dir() {
        let dir = scratch("both");
        std::fs::create_dir_all(dir.join(".rag")).unwrap();
        std::fs::write(dir.join(".rag").join("config.toml"), "").unwrap();
        let project = canonical(&dir).unwrap();

        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        reg.upsert(&project);

        // A migrated project must not fall back to the `.rag/` left behind.
        assert!(!ProjectPaths::resolve_with(&reg, &project).is_legacy());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn project_local_queries_still_override() {
        let dir = scratch("queries");
        let project = canonical(&dir).unwrap();
        let mut reg = Registry::load_from(&dir.join("registry.json")).unwrap();
        let id = reg.upsert(&project).id.clone();

        // With no `.rag/queries/`, the global slot is used…
        let paths = ProjectPaths::resolve_with(&reg, &project);
        assert_eq!(paths.queries(), project_dir(&id).join("queries"));

        // …and a hand-authored project-local override still wins.
        std::fs::create_dir_all(project.join(".rag").join("queries")).unwrap();
        assert_eq!(paths.queries(), project.join(".rag").join("queries"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collection_naming_follows_the_layout() {
        let global = ProjectPaths {
            root: PathBuf::from("/p/my-app"),
            data: PathBuf::from("/d/my-app-1234abcd"),
            id: Some("my-app-1234abcd".into()),
        };
        let legacy = ProjectPaths {
            root: PathBuf::from("/p/my-app"),
            data: PathBuf::from("/p/my-app/.rag"),
            id: None,
        };

        // Global: the id names the collection.
        assert_eq!(global.collection(None, "My App"), "my-app-1234abcd");
        // Legacy: the old lowercased project name.
        assert_eq!(legacy.collection(None, "My App"), "my_app");
        // An explicit `[qdrant] collection` wins on both layouts.
        assert_eq!(global.collection(Some("Team Index"), "My App"), "team_index");
        assert_eq!(legacy.collection(Some("Team Index"), "My App"), "team_index");
        // The guard looks for what the collection used to be called.
        assert_eq!(global.legacy_collection("My App"), "my_app");
    }
}

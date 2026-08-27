//! The compiler: raw daily logs and inbox drops become knowledge notes.
//!
//! Three guarantees shape the whole design:
//!
//! * **It never deletes.** New information that contradicts an existing note is
//!   marked, not applied — the human decides which one was wrong.
//! * **It never writes half a chunk.** Output that does not parse is skipped
//!   whole and reported, so a confused model cannot leave a note mangled.
//! * **Everything it does is one `git revert` away.** Each run commits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};

use super::config::BrainConfig;
use super::engine::{self, CompileRequest};
use super::{inbox_dir, knowledge_dir, skills_dir};

/// Where the model's output is written before anything real is touched.
const STAGING: &str = ".staging";
/// Bookkeeping: what has already been compiled.
const STATE_FILE: &str = ".compile.json";
/// Share of the daily budget one chunk of raw material may take.
const CHUNK_SHARE: f64 = 0.2;

/// The compiler instruction. Line- and block-prefixed rather than JSON: a cheap
/// model keeps prefixes straight far more reliably than balanced braces, and a
/// malformed block costs one note instead of the whole run.
const COMPILER_PROMPT: &str = "\
You are a knowledge compiler for a personal second brain. You are given raw
session logs and dropped-in material. Distil them into durable notes.

Answer using ONLY these blocks, in this exact shape:

NOTE: <kebab-case-slug>
TAGS: <comma separated, may be empty>
LINKS: <comma separated slugs of related notes, may be empty>
BODY:
<markdown, any number of lines>
END

SKILL: <kebab-case-slug>
BODY:
<a repeatable procedure, written as steps>
END

CONTRADICTS: <existing-note-slug> | <what the new material says that conflicts>

The material begins with an EXISTING NOTES index: the slugs already in the
brain, with their titles. Use it.

Rules:
- Emit a NOTE only for something worth remembering weeks from now. Ephemeral
  chatter is not worth a note.
- One subject per note. If the subject already has a slug in the index, REUSE
  that slug — your body is appended to that note. Only invent a slug for a
  subject the index does not cover.
- Write SKILL blocks only for a procedure that was repeated or explicitly
  described as a way of working.
- Use CONTRADICTS when the new material disagrees with an existing note. Never
  rewrite the old claim yourself.
- Record only what the material supports. Invent nothing.
- No preamble, no commentary, no code fences around the blocks.";

// ── entry point ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct CompileReport {
    pub sources: Vec<String>,
    pub notes_created: Vec<String>,
    pub notes_updated: Vec<String>,
    pub skills: Vec<String>,
    pub contradictions: Vec<String>,
    pub skipped_chunks: Vec<String>,
    pub committed: bool,
}

/// Compile everything new. `light` restricts the run to today's daily — the
/// cheap end-of-session pass rather than the full nightly one.
/// One model call: digest raw material into staged notes.
///
/// Shared by `compile` and `import` so both go through the same parser, the
/// same staging discipline and the same never-delete rules.
pub(super) struct Distiller<'a> {
    engine: &'a dyn super::engine::CompilerEngine,
    staging: PathBuf,
}

impl<'a> Distiller<'a> {
    pub(super) fn new(engine: &'a dyn super::engine::CompilerEngine) -> Self {
        let staging = super::dir().join(STAGING);
        let _ = std::fs::remove_dir_all(&staging);
        Self { engine, staging }
    }

    /// The slugs already in the brain, prepended to every call.
    pub(super) fn index_header(&self) -> String {
        existing_index()
    }

    /// Digest one chunk. A chunk whose output does not parse is skipped whole
    /// and recorded in the report — never half-applied.
    pub(super) async fn digest(
        &self,
        label: &str,
        input: &str,
        report: &mut CompileReport,
    ) -> Result<()> {
        let raw = match self
            .engine
            .complete(CompileRequest {
                system: COMPILER_PROMPT,
                input,
                max_output_tokens: 4_000,
            })
            .await
        {
            Ok(text) => text,
            Err(e) => {
                report.skipped_chunks.push(format!("{label}: {e}"));
                return Ok(());
            }
        };

        match Output::parse(&raw) {
            Ok(output) if output.is_empty() => Ok(()),
            Ok(output) => apply(&output, &self.staging, report),
            Err(why) => {
                report.skipped_chunks.push(format!("{label}: {why}"));
                Ok(())
            }
        }
    }

    /// Move everything staged into the vault. Called once, after every chunk
    /// has been through `digest`.
    pub(super) fn finish(self) -> Result<()> {
        promote(&self.staging)?;
        let _ = std::fs::remove_dir_all(&self.staging);
        Ok(())
    }
}

/// Index the vault and commit — the tail every distilling run shares.
pub(super) async fn index_and_commit() -> bool {
    if let Ok(rt) = super::runtime::runtime().await {
        let _ = rt.orchestrator.ensure_index(false).await;
    }
    commit()
}

pub async fn cmd_compile(light: bool, engine_override: Option<&str>) -> Result<()> {
    if !super::exists() {
        anyhow::bail!("No brain at {} — run `ragpilot brain init` first.", super::dir().display());
    }
    let cfg = BrainConfig::load(&super::config_path())?;
    let report = run(&cfg, light, engine_override).await?;
    print_report(&report);
    Ok(())
}

async fn run(cfg: &BrainConfig, light: bool, engine_override: Option<&str>) -> Result<CompileReport> {
    let mut report = CompileReport::default();

    let mut state = CompileState::load();
    let pending = gather(&state, light)?;
    if pending.is_empty() {
        println!("{} Nothing new to compile.", "i".blue());
        return Ok(report);
    }
    report.sources = pending.iter().map(|s| s.label.clone()).collect();

    let engine = engine::create(cfg, engine_override).map_err(|e| anyhow::anyhow!("{e}"))?;
    engine
        .available()
        .map_err(|e| anyhow::anyhow!("Compiler engine '{}' unavailable: {e}", engine.name()))?;

    let chunk_budget = (cfg.compiler.daily_token_budget as f64 * CHUNK_SHARE) as usize;
    let index = existing_index();
    let chunks: Vec<Chunk> = chunk(&pending, chunk_budget.max(1_000), cfg.compiler.daily_token_budget)
        .into_iter()
        .map(|c| Chunk { body: format!("{index}\n{}", c.body), sources: c.sources })
        .collect();
    println!(
        "{} Compiling {} source(s) in {} chunk(s) with '{}'…",
        "→".cyan(),
        pending.len(),
        chunks.len(),
        engine.name()
    );

    let distiller = Distiller::new(engine.as_ref());
    let total = chunks.len();
    let mut compiled: Vec<usize> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let before = report.skipped_chunks.len();
        distiller
            .digest(&format!("chunk {}/{total}", i + 1), &chunk.body, &mut report)
            .await?;
        // A chunk that was skipped leaves its sources unrecorded, so the next
        // run picks them up again instead of losing the material silently.
        if report.skipped_chunks.len() == before {
            compiled.extend(chunk.sources.iter().copied());
        }
    }
    distiller.finish()?;

    // Only mark sources compiled once their output is safely on disk.
    for i in compiled {
        state.record(&pending[i].path, &pending[i].hash);
    }
    state.save();

    report.committed = index_and_commit().await;
    Ok(report)
}

// ── gathering ──────────────────────────────────────────────────────────────

struct Source {
    path: PathBuf,
    label: String,
    hash: String,
    text: String,
}

/// Daily logs and inbox drops that changed since the last compile.
fn gather(state: &CompileState, light: bool) -> Result<Vec<Source>> {
    let mut roots = vec![super::daily_dir()];
    if !light {
        roots.push(inbox_dir());
    }

    let today = super::vault::today();
    let mut out = Vec::new();

    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        paths.sort();

        for path in paths {
            let label = relative(&path);
            if light && !label.contains(&today) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if text.trim().is_empty() {
                continue;
            }
            let hash = format!("{:x}", md5::compute(text.as_bytes()));
            if state.is_current(&path, &hash) {
                continue;
            }
            out.push(Source { path, label, hash, text });
        }
    }
    Ok(out)
}

/// One model call's worth of raw material, and which sources went into it.
struct Chunk {
    body: String,
    /// Indices into the `sources` slice — so a skipped chunk can leave exactly
    /// its own sources unrecorded.
    sources: Vec<usize>,
}

/// Split the raw material into model-sized chunks, stopping at the daily
/// budget. Sources are never split mid-file — a note built from half a day's
/// log loses the half that explained it.
fn chunk(sources: &[Source], chunk_budget: usize, total_budget: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current = Chunk { body: String::new(), sources: Vec::new() };
    let mut spent = 0usize;

    for (i, source) in sources.iter().enumerate() {
        let piece = format!("--- source: {} ---\n{}\n", source.label, source.text.trim());
        let cost = crate::tokens::estimate(&piece);
        if spent + cost > total_budget {
            break;
        }
        spent += cost;

        if !current.body.is_empty() && crate::tokens::estimate(&current.body) + cost > chunk_budget {
            chunks.push(std::mem::replace(
                &mut current,
                Chunk { body: String::new(), sources: Vec::new() },
            ));
        }
        current.body.push_str(&piece);
        current.sources.push(i);
    }
    if !current.body.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Slugs that exist as notes right now, for filtering the model's links.
fn existing_slugs() -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for dir in [knowledge_dir(), skills_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                if let Some(stem) = path.file_stem() {
                    out.insert(stem.to_string_lossy().to_string());
                }
            }
        }
    }
    out
}

/// The slugs already in the brain, with their titles.
///
/// Without this the model cannot reuse a slug (so every run would create a new
/// note instead of extending one) and cannot possibly know what it is
/// contradicting. It is prepended to every chunk, because each chunk is an
/// independent call with no memory of the others.
fn existing_index() -> String {
    const MAX_ENTRIES: usize = 150;
    let mut lines = Vec::new();

    for (area, dir) in [("knowledge", knowledge_dir()), ("skills", skills_dir())] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        paths.sort();

        for path in paths {
            let Some(slug) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            let title = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find(|l| l.starts_with("# "))
                        .map(|l| l.trim_start_matches("# ").trim().to_string())
                })
                .unwrap_or_else(|| title_from(&slug));
            // The slug stands alone. Writing it as `knowledge/<slug>` reads as
            // part of the name to a model that is being asked for a slug, and
            // it dutifully answers `knowledge/<slug>` — which then slugifies
            // into a `knowledge-` prefixed duplicate of the note it meant.
            lines.push(format!("- {slug}  ({area}) — {title}"));
        }
    }

    if lines.is_empty() {
        return "--- EXISTING NOTES ---\n(the brain has no notes yet)\n".to_string();
    }
    let total = lines.len();
    lines.truncate(MAX_ENTRIES);
    let more = if total > MAX_ENTRIES {
        format!("\n({} more not listed)", total - MAX_ENTRIES)
    } else {
        String::new()
    };
    format!("--- EXISTING NOTES ---\n{}{more}\n", lines.join("\n"))
}

// ── the model's answer ─────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
struct Note {
    slug: String,
    tags: Vec<String>,
    links: Vec<String>,
    body: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Output {
    notes: Vec<Note>,
    skills: Vec<Note>,
    contradictions: Vec<(String, String)>,
}

impl Output {
    fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.skills.is_empty() && self.contradictions.is_empty()
    }

    /// Parse the block format. An unterminated block is an error for the whole
    /// chunk — half a note is worse than no note.
    fn parse(raw: &str) -> Result<Self, String> {
        let mut out = Self::default();
        let mut lines = raw.lines().peekable();

        while let Some(line) = lines.next() {
            let trimmed = line.trim();

            if let Some(rest) = trimmed.strip_prefix("CONTRADICTS:") {
                let (slug, what) = rest
                    .split_once('|')
                    .ok_or_else(|| "CONTRADICTS without a '|' separator".to_string())?;
                let (slug, what) = (slugify(slug), what.trim().to_string());
                if slug.is_empty() || what.is_empty() {
                    return Err("CONTRADICTS with an empty slug or description".into());
                }
                out.contradictions.push((slug, what));
                continue;
            }

            let (kind, rest) = if let Some(rest) = trimmed.strip_prefix("NOTE:") {
                ("note", rest)
            } else if let Some(rest) = trimmed.strip_prefix("SKILL:") {
                ("skill", rest)
            } else {
                continue;
            };

            let slug = slugify(rest);
            if slug.is_empty() {
                return Err(format!("{kind} block with an empty slug"));
            }
            let mut note = Note { slug, ..Default::default() };

            // Headers, then BODY … END.
            let mut in_body = false;
            let mut closed = false;
            for line in lines.by_ref() {
                let trimmed = line.trim();
                if !in_body {
                    if let Some(rest) = trimmed.strip_prefix("TAGS:") {
                        note.tags = split_list(rest);
                    } else if let Some(rest) = trimmed.strip_prefix("LINKS:") {
                        note.links = split_list(rest).iter().map(|l| slugify(l)).collect();
                    } else if trimmed == "BODY:" {
                        in_body = true;
                    }
                    continue;
                }
                if trimmed == "END" {
                    closed = true;
                    break;
                }
                note.body.push_str(line);
                note.body.push('\n');
            }

            if !closed {
                return Err(format!("{kind} '{}' was never closed with END", note.slug));
            }
            if note.body.trim().is_empty() {
                return Err(format!("{kind} '{}' has an empty body", note.slug));
            }
            note.links.retain(|l| !l.is_empty() && *l != note.slug);

            if kind == "note" {
                out.notes.push(note);
            } else {
                out.skills.push(note);
            }
        }
        Ok(out)
    }
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Reduce free text to a file-name-safe slug.
///
/// An area prefix is dropped first: a model handed an index of notes sometimes
/// answers with the path it was shown rather than the name, and
/// `knowledge/foo` must resolve to `foo`, not to a new note called
/// `knowledge-foo` sitting beside it.
fn slugify(raw: &str) -> String {
    let raw = raw.trim();
    // Only the path form is stripped. `knowledge-something` is a perfectly
    // good slug for a note about knowledge, and cutting it would invent a
    // different note than the one meant.
    let raw = ["knowledge/", "skills/"]
        .iter()
        .find_map(|p| raw.strip_prefix(p))
        .unwrap_or(raw);

    let mut out = String::new();
    let mut dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

// ── applying ───────────────────────────────────────────────────────────────

/// Render every planned change into the staging directory. Existing notes are
/// copied in first and **appended to** — a note is never rewritten from
/// scratch, so nothing the user or an earlier compile wrote can be lost.
fn apply(output: &Output, staging: &Path, report: &mut CompileReport) -> Result<()> {
    // A link may point at a note that already exists, or at one this run is
    // about to write. Anything else is the model guessing at a name, and a
    // guess that ships is a broken link the user has to clean up by hand.
    let mut known = existing_slugs();
    known.extend(output.notes.iter().map(|n| n.slug.clone()));
    known.extend(output.skills.iter().map(|n| n.slug.clone()));

    for note in &output.notes {
        let created = stage_note(staging, &knowledge_dir(), note, "knowledge", &known)?;
        if created {
            report.notes_created.push(note.slug.clone());
        } else {
            report.notes_updated.push(note.slug.clone());
        }
    }
    for skill in &output.skills {
        stage_note(staging, &skills_dir(), skill, "skills", &known)?;
        report.skills.push(skill.slug.clone());
    }
    for (slug, what) in &output.contradictions {
        stage_contradiction(staging, slug, what)?;
        report.contradictions.push(format!("{slug}: {what}"));
    }
    Ok(())
}

/// Returns whether the note is new.
fn stage_note(
    staging: &Path,
    real_dir: &Path,
    note: &Note,
    area: &str,
    known: &std::collections::BTreeSet<String>,
) -> Result<bool> {
    let file = format!("{}.md", note.slug);
    let live = real_dir.join(&file);
    let staged = staging.join(area).join(&file);
    std::fs::create_dir_all(staged.parent().unwrap())?;

    let today = super::vault::today();
    let existing = std::fs::read_to_string(&staged)
        .or_else(|_| std::fs::read_to_string(&live))
        .ok();
    let is_new = existing.is_none();

    let mut text = match existing {
        Some(text) => bump_updated(&text, &today),
        None => format!(
            "---\ntags: [{}]\nsource: compiler\nupdated: {today}\n---\n\n# {}\n",
            note.tags.join(", "),
            title_from(&note.slug),
        ),
    };

    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("\n## {today}\n\n{}\n", note.body.trim()));

    if !note.links.is_empty() {
        let links: Vec<String> = note
            .links
            .iter()
            .filter(|l| known.contains(*l))
            .filter(|l| !text.contains(&format!("[[{l}]]")))
            .map(|l| format!("[[{l}]]"))
            .collect();
        if !links.is_empty() {
            text.push_str(&format!("\nRelated: {}\n", links.join(" ")));
        }
    }

    std::fs::write(&staged, text)?;
    Ok(is_new)
}

/// A contradiction is a comment on the existing note, never an edit of it.
fn stage_contradiction(staging: &Path, slug: &str, what: &str) -> Result<()> {
    let file = format!("{slug}.md");
    let live = knowledge_dir().join(&file);
    let staged = staging.join("knowledge").join(&file);
    std::fs::create_dir_all(staged.parent().unwrap())?;

    let existing = std::fs::read_to_string(&staged)
        .or_else(|_| std::fs::read_to_string(&live))
        .ok();
    // A contradiction against a note that does not exist is not worth
    // inventing a note for; the report still carries it.
    let Some(mut text) = existing else { return Ok(()) };

    let marker = format!("> ⚠ Çelişki ({}): {what}", super::vault::today());
    if text.contains(&marker) {
        return Ok(());
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("\n{marker}\n"));
    std::fs::write(&staged, text)?;
    Ok(())
}

/// Move staged files into the vault. Runs only after every chunk has been
/// parsed and rendered, so the vault never sees a partial run.
fn promote(staging: &Path) -> Result<()> {
    if !staging.exists() {
        return Ok(());
    }
    for area in ["knowledge", "skills"] {
        let from_dir = staging.join(area);
        let Ok(entries) = std::fs::read_dir(&from_dir) else { continue };
        let to_dir = super::dir().join(area);
        std::fs::create_dir_all(&to_dir)?;
        for entry in entries.flatten() {
            let to = to_dir.join(entry.file_name());
            std::fs::copy(entry.path(), &to)
                .with_context(|| format!("Cannot write {}", to.display()))?;
        }
    }
    Ok(())
}

fn bump_updated(text: &str, today: &str) -> String {
    let mut seen_frontmatter = false;
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("updated:") && !seen_frontmatter {
                seen_frontmatter = true;
                format!("updated: {today}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn title_from(slug: &str) -> String {
    slug.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn relative(path: &Path) -> String {
    path.strip_prefix(super::dir())
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// Commit the run. Best-effort: a vault without git still compiles.
fn commit() -> bool {
    let root = super::dir();
    let added = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !added {
        return false;
    }
    std::process::Command::new("git")
        .args(["commit", "-q", "-m", &format!("compile: {}", super::vault::today())])
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(super) fn print_report(r: &CompileReport) {
    if r.sources.is_empty() {
        return;
    }
    println!("\n{}", "─── Compile report ──────────────────────────────".bold());
    println!("  sources:        {}", r.sources.join(", "));
    println!("  notes created:  {}", list(&r.notes_created));
    println!("  notes updated:  {}", list(&r.notes_updated));
    println!("  skills:         {}", list(&r.skills));
    println!("  contradictions: {}", list(&r.contradictions));
    if !r.skipped_chunks.is_empty() {
        println!("  {} {}", "skipped:".yellow(), r.skipped_chunks.join("; "));
    }
    println!("  committed:      {}", if r.committed { "yes" } else { "no" });
}

fn list(items: &[String]) -> String {
    if items.is_empty() { "—".to_string() } else { items.join(", ") }
}

// ── what has already been compiled ─────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct CompileState {
    #[serde(default)]
    sources: BTreeMap<String, String>,
}

impl CompileState {
    fn path() -> PathBuf { super::dir().join(STATE_FILE) }

    fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), body);
        }
    }

    fn is_current(&self, path: &Path, hash: &str) -> bool {
        self.sources.get(&relative(path)).is_some_and(|h| h == hash)
    }

    fn record(&mut self, path: &Path, hash: &str) {
        self.sources.insert(relative(path), hash.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "\
NOTE: qdrant-alias-migration
TAGS: ragpilot, qdrant
LINKS: project-registry, qdrant-alias-migration
BODY:
Migrating a project aliases the old collection to the new id.
No re-embedding happens.
END

SKILL: verify-a-phase
BODY:
1. Run the tests.
2. Smoke the real binary.
END

CONTRADICTS: markdown-source-of-truth | the new material claims Qdrant is authoritative
";

    #[test]
    fn parses_notes_skills_and_contradictions() {
        let out = Output::parse(GOOD).unwrap();

        assert_eq!(out.notes.len(), 1);
        let note = &out.notes[0];
        assert_eq!(note.slug, "qdrant-alias-migration");
        assert_eq!(note.tags, vec!["ragpilot", "qdrant"]);
        // A self-link is dropped rather than written as a loop.
        assert_eq!(note.links, vec!["project-registry"]);
        assert!(note.body.contains("No re-embedding"));

        assert_eq!(out.skills.len(), 1);
        assert_eq!(out.skills[0].slug, "verify-a-phase");

        assert_eq!(out.contradictions.len(), 1);
        assert_eq!(out.contradictions[0].0, "markdown-source-of-truth");
        assert!(out.contradictions[0].1.contains("Qdrant is authoritative"));
    }

    #[test]
    fn a_malformed_block_fails_the_whole_chunk() {
        // No END: half a note is worse than no note.
        assert!(Output::parse("NOTE: x\nBODY:\nsomething").is_err());
        // Empty body.
        assert!(Output::parse("NOTE: x\nBODY:\nEND").is_err());
        // No slug.
        assert!(Output::parse("NOTE:\nBODY:\nthing\nEND").is_err());
        // Malformed contradiction.
        assert!(Output::parse("CONTRADICTS: no separator here").is_err());
    }

    #[test]
    fn commentary_around_the_blocks_is_ignored() {
        let noisy = format!("Sure! Here you go:\n\n{GOOD}\n\nLet me know if you need more.");
        let out = Output::parse(&noisy).unwrap();
        assert_eq!(out.notes.len(), 1);
        assert_eq!(out.skills.len(), 1);

        // Nothing at all is valid and empty, not an error.
        let nothing = Output::parse("I found nothing worth recording.").unwrap();
        assert!(nothing.is_empty());
    }

    #[test]
    fn slugify_drops_an_area_prefix_the_model_echoed_back() {
        // The shape that created five duplicate notes before the index format
        // was fixed: the model answered with what it was shown.
        assert_eq!(slugify("knowledge/enterprise-core-binary-deployment"), "enterprise-core-binary-deployment");
        assert_eq!(slugify("skills/verify-a-phase"), "verify-a-phase");
        // A slug that merely begins with the word is left alone — cutting it
        // would invent a different note than the one meant.
        assert_eq!(slugify("knowledge-brain-rules"), "knowledge-brain-rules");
        assert_eq!(slugify("knowledge base design"), "knowledge-base-design");
    }

    #[test]
    fn the_existing_index_lists_slugs_not_paths() {
        // Regression: `knowledge/<slug>` in the index reads as the name.
        let line = format!("- {}  ({}) — {}", "a-note", "knowledge", "A Note");
        assert!(line.starts_with("- a-note"), "{line}");
        assert!(!line.contains("knowledge/"), "{line}");
    }

    #[test]
    fn slugify_makes_safe_file_names() {
        assert_eq!(slugify(" Qdrant Alias Migration "), "qdrant-alias-migration");
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("a/b\\c"), "a-b-c");
        assert_eq!(slugify("!!!"), "");
        assert!(slugify(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn bump_updated_touches_only_the_frontmatter_date() {
        let text = "---\ntags: [a]\nupdated: 2026-01-01\n---\n\n# T\n\nupdated: not frontmatter\n";
        let out = bump_updated(text, "2026-08-27");
        assert!(out.contains("updated: 2026-08-27"));
        assert!(out.contains("updated: not frontmatter"), "body line was rewritten");
        assert_eq!(out.matches("updated: 2026-08-27").count(), 1);
    }

    #[test]
    fn the_existing_index_is_prepended_to_every_chunk() {
        // Each chunk is an independent model call, so the index cannot be sent
        // once and assumed remembered.
        let index = "--- EXISTING NOTES ---\n- knowledge/a — A\n";
        let sources: Vec<Source> = (0..4)
            .map(|i| Source {
                path: PathBuf::from(format!("daily/{i}.md")),
                label: format!("daily/{i}.md"),
                hash: String::new(),
                text: (0..300).map(|j| format!("line {i}-{j}")).collect::<Vec<_>>().join("\n"),
            })
            .collect();

        let chunks: Vec<String> = chunk(&sources, 2_000, 100_000)
            .into_iter()
            .map(|c| format!("{index}\n{}", c.body))
            .collect();

        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.starts_with("--- EXISTING NOTES ---"), "a chunk went out without the index");
        }
    }

    #[test]
    fn chunking_respects_both_budgets_and_never_splits_a_source() {
        let sources: Vec<Source> = (0..6)
            .map(|i| {
                let text = (0..200).map(|j| format!("line {i}-{j}")).collect::<Vec<_>>().join("\n");
                Source {
                    path: PathBuf::from(format!("daily/{i}.md")),
                    label: format!("daily/{i}.md"),
                    hash: String::new(),
                    text,
                }
            })
            .collect();

        let chunks = chunk(&sources, 2_000, 100_000);
        assert!(chunks.len() > 1, "everything landed in one chunk");
        for c in &chunks {
            // A single source may exceed the chunk budget; it is still never split.
            assert!(c.body.contains("--- source:"));
            assert!(!c.sources.is_empty(), "a chunk with no source attribution");
        }
        // Every source is attributed to exactly one chunk — the basis for
        // retrying only what was skipped.
        let mut attributed: Vec<usize> = chunks.iter().flat_map(|c| c.sources.clone()).collect();
        attributed.sort();
        assert_eq!(attributed, (0..sources.len()).collect::<Vec<_>>());

        // The total budget stops the run early rather than overspending.
        let capped = chunk(&sources, 2_000, 3_000);
        let spent: usize = capped.iter().map(|c| crate::tokens::estimate(&c.body)).sum();
        assert!(spent <= 3_000, "total budget exceeded: {spent}");
        // …and the sources that never made it are simply not attributed.
        let carried: usize = capped.iter().map(|c| c.sources.len()).sum();
        assert!(carried < sources.len(), "budget cap attributed every source");
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ragpilot-links-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_links_to_notes_that_exist_are_written() {
        let dir = scratch("filter");
        let real = dir.join("knowledge");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("already-here.md"), "# Already Here\n").unwrap();

        let note = Note {
            slug: "new-note".into(),
            tags: vec!["a".into()],
            links: vec![
                "already-here".into(),   // exists
                "written-this-run".into(), // being created alongside
                "invented-by-the-model".into(), // neither
            ],
            body: "Something worth keeping.".into(),
        };

        let known: BTreeSet<String> = ["already-here", "written-this-run", "new-note"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let staging = dir.join(".staging");
        stage_note(&staging, &real, &note, "knowledge", &known).unwrap();
        let text = std::fs::read_to_string(staging.join("knowledge").join("new-note.md")).unwrap();

        assert!(text.contains("[[already-here]]"));
        assert!(text.contains("[[written-this-run]]"));
        // The one the model made up never reaches the vault.
        assert!(!text.contains("invented-by-the-model"), "{text}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_with_no_resolvable_links_gets_no_related_line() {
        let dir = scratch("none");
        let real = dir.join("knowledge");
        std::fs::create_dir_all(&real).unwrap();

        let note = Note {
            slug: "lonely".into(),
            tags: vec![],
            links: vec!["nothing-real".into()],
            body: "Body.".into(),
        };
        let staging = dir.join(".staging");
        stage_note(&staging, &real, &note, "knowledge", &BTreeSet::new()).unwrap();

        let text = std::fs::read_to_string(staging.join("knowledge").join("lonely.md")).unwrap();
        assert!(!text.contains("Related:"), "an empty Related line was written: {text}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

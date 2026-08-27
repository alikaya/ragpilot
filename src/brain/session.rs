//! Session hooks: what runs when an agent session opens and closes.
//!
//! Opening prints the brain's context to stdout, which Claude Code injects
//! straight into the conversation — the agent does not have to *choose* to
//! remember. Closing digests the transcript with the cheap compiler model and
//! appends a session block to today's daily.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::config::BrainConfig;
use super::engine::{self, CompileRequest};
use super::vault;

/// Instruction handed to the compiler model. Deliberately line-prefixed rather
/// than JSON: a small, cheap model gets prefixes right far more reliably than
/// balanced braces, and a malformed line costs one bullet instead of the whole
/// flush.
const SUMMARY_PROMPT: &str = "\
You are a session log compiler. You are given the raw transcript of a coding \
session. Produce a compact record of it using EXACTLY this line format, and \
nothing else:

SUMMARY: <one or two sentences on what actually happened>
DECISION: <a decision that was made and should hold in future sessions>
OPEN: <something started but not finished, or explicitly deferred>

Rules:
- Exactly one SUMMARY line. Zero or more DECISION and OPEN lines.
- Write each on a single line. No markdown, no bullets, no preamble.
- Record only what the transcript supports. Invent nothing.
- Prefer the user's own words for decisions.
- If nothing was decided, emit no DECISION lines. Same for OPEN.";

/// How much of a transcript is handed to the model in one go, as a fraction of
/// the configured daily budget.
const TRANSCRIPT_SHARE: f64 = 0.25;

/// Left behind when a session ends without its record reaching disk, and
/// surfaced at the start of the next one.
///
/// The hooks make the write a mechanism rather than a discipline, but a
/// mechanism can still fail — no compiler engine, no network, a model that
/// answers nothing. Without a marker that failure is silent, and a day of work
/// is simply gone.
fn marker_path() -> PathBuf { super::dir().join(".missed-flush") }

fn record_miss(reason: &str) {
    let line = format!("{} — {reason}\n", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    let _ = std::fs::write(marker_path(), line);
}

fn clear_miss() {
    let _ = std::fs::remove_file(marker_path());
}

/// The pending miss, if there is one. Reading it clears it: it is a nudge for
/// the next session, not a permanent record.
fn take_miss() -> Option<String> {
    let text = std::fs::read_to_string(marker_path()).ok()?;
    clear_miss();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

// ── session-start ──────────────────────────────────────────────────────────

/// Print the opening package. Stdout is the contract: Claude Code injects it,
/// and a human reading it sees the same thing.
pub fn cmd_session_start(max_tokens: Option<usize>) -> Result<()> {
    if !super::exists() {
        // A missing brain must not make a session fail to start.
        eprintln!("ragpilot: no brain yet — run `ragpilot brain init` to set one up.");
        return Ok(());
    }
    let cfg = BrainConfig::load(&super::config_path())?;
    let budget = max_tokens.unwrap_or(cfg.load.max_tokens).max(1);

    if let Some(miss) = take_miss() {
        println!(
            "## Unrecorded session\n\nThe previous session ended without its record reaching \
             the brain ({miss}). If you can reconstruct what happened — from the repository, \
             the transcript, or by asking — note it now with `brain_note`, then carry on.\n"
        );
    }
    print!("{}", vault::load_package(budget)?);
    Ok(())
}

// ── session-end ────────────────────────────────────────────────────────────

/// Digest a transcript and write the session block.
///
/// `transcript` may come from the flag or from the hook payload on stdin.
/// Called by both the `SessionEnd` and `PreCompact` hooks: only the part of the
/// transcript not yet flushed is processed, so the two never duplicate work.
pub async fn cmd_session_end(
    transcript: Option<PathBuf>,
    engine_override: Option<&str>,
) -> Result<()> {
    if !super::exists() {
        return Ok(());
    }

    let Some(path) = transcript.or_else(transcript_from_stdin) else {
        eprintln!("ragpilot: no transcript path given — nothing to flush.");
        return Ok(());
    };
    if !path.exists() {
        eprintln!("ragpilot: transcript {} does not exist.", path.display());
        return Ok(());
    }

    let mut state = SessionState::load();
    let seen = state.processed(&path);
    let (text, total_lines) = read_transcript(&path, seen)?;

    if text.trim().is_empty() {
        // Nothing new since the last flush — the common case when PreCompact
        // and SessionEnd both fire.
        state.record(&path, total_lines);
        state.save();
        return Ok(());
    }

    let cfg = BrainConfig::load(&super::config_path())?;
    let budget = (cfg.compiler.daily_token_budget as f64 * TRANSCRIPT_SHARE) as usize;
    let input = truncate_tokens(&text, budget.max(500));

    // From here on every failure leaves a marker, so the next session knows the
    // day was lost instead of assuming it was quiet.
    let digest = match summarise(&cfg, engine_override, &input).await {
        Ok(digest) => digest,
        Err(e) => {
            record_miss(&e.to_string());
            return Err(e);
        }
    };

    let written = match vault::append_flush(&digest.summary, &digest.decisions, &digest.open_threads)
        .and_then(|p| vault::update_threads(&digest.open_threads, &[]).map(|_| p))
    {
        Ok(p) => p,
        Err(e) => {
            record_miss(&format!("could not write the session block: {e}"));
            return Err(e);
        }
    };
    clear_miss();
    state.record(&path, total_lines);
    state.save();

    // Index so the block is searchable immediately. A failure here leaves the
    // markdown intact, which is the part that matters.
    if let Ok(rt) = super::runtime::runtime().await {
        let _ = rt.index_file(&written).await;
    }

    eprintln!(
        "ragpilot: session flushed to {} ({} decision(s), {} open thread(s))",
        written.display(),
        digest.decisions.len(),
        digest.open_threads.len()
    );
    Ok(())
}

/// One compiler call, turned into a digest. Split out so every failure on the
/// way there is caught in one place.
async fn summarise(
    cfg: &BrainConfig,
    engine_override: Option<&str>,
    input: &str,
) -> Result<Digest> {
    // A session summary is the easy half of the job, so it can run on a cheaper
    // model than the nightly compile when the user asks for that.
    let engine = engine::create_with_model(cfg, engine_override, cfg.flush_model())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    engine
        .available()
        .map_err(|e| anyhow::anyhow!("compiler engine '{}' unavailable: {e}", engine.name()))?;

    let raw = engine
        .complete(CompileRequest {
            system: SUMMARY_PROMPT,
            input,
            max_output_tokens: 800,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let digest = Digest::parse(&raw);
    if digest.summary.trim().is_empty() {
        anyhow::bail!("the compiler produced no SUMMARY line");
    }
    Ok(digest)
}

/// Hook payloads arrive as JSON on stdin; `transcript_path` is the field we
/// need. Absent or unparseable stdin is not an error — the flag may have been
/// used instead.
fn transcript_from_stdin() -> Option<PathBuf> {
    use std::io::{IsTerminal, Read};

    // A hook is fed its payload on a pipe. Run by hand in a terminal there is
    // no payload coming, and reading to EOF would simply hang.
    if std::io::stdin().is_terminal() {
        return None;
    }

    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    let payload: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    payload
        .get("transcript_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Read the transcript from line `skip` onward, flattening it to plain text.
/// Returns the text and the total line count, so the next run knows where to
/// resume.
fn read_transcript(path: &Path, skip: usize) -> Result<(String, usize)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read transcript {}", path.display()))?;

    let lines: Vec<&str> = raw.lines().collect();
    let total = lines.len();
    let text = lines
        .iter()
        .skip(skip)
        .filter_map(|line| entry_text(line))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok((text, total))
}

/// Flatten one transcript entry to `role: text`, or `None` when it carries no
/// conversational text (tool results, metadata, malformed lines).
fn entry_text(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let message = value.get("message").unwrap_or(&value);
    let role = message.get("role").and_then(|r| r.as_str())?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }

    let content = message.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };

    let text = text.trim();
    if text.is_empty() { None } else { Some(format!("{role}: {text}")) }
}

/// Keep the **tail** of a long transcript: the end of a session is where the
/// conclusions and the unfinished work are.
fn truncate_tokens(text: &str, budget: usize) -> String {
    if crate::tokens::estimate(text) <= budget {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    for line in lines.iter().rev() {
        kept.push(line);
        let candidate = kept.iter().rev().copied().collect::<Vec<_>>().join("\n");
        if crate::tokens::estimate(&candidate) > budget {
            kept.pop();
            break;
        }
    }
    kept.reverse();
    format!("[earlier turns omitted]\n{}", kept.join("\n"))
}

// ── the compiler's answer ──────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
struct Digest {
    summary: String,
    decisions: Vec<String>,
    open_threads: Vec<String>,
}

impl Digest {
    /// Pick the prefixed lines out of the model's reply. Anything else — a
    /// preamble, a stray bullet, a markdown fence — is ignored rather than
    /// treated as content.
    fn parse(raw: &str) -> Self {
        let mut digest = Self::default();
        for line in raw.lines() {
            let line = line.trim().trim_start_matches(['-', '*', '#']).trim();
            if let Some(rest) = strip_label(line, "SUMMARY") {
                if digest.summary.is_empty() {
                    digest.summary = rest;
                }
            } else if let Some(rest) = strip_label(line, "DECISION") {
                digest.decisions.push(rest);
            } else if let Some(rest) = strip_label(line, "OPEN") {
                digest.open_threads.push(rest);
            }
        }
        digest
    }
}

fn strip_label(line: &str, label: &str) -> Option<String> {
    let rest = line.strip_prefix(label)?.trim_start();
    let rest = rest.strip_prefix(':')?.trim();
    if rest.is_empty() { None } else { Some(rest.to_string()) }
}

// ── how much of each transcript is already flushed ─────────────────────────

/// Derived bookkeeping, not vault content: gitignored, and safe to delete —
/// the worst a lost file causes is one duplicate summary.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionState {
    #[serde(default)]
    transcripts: BTreeMap<String, usize>,
}

impl SessionState {
    fn path() -> PathBuf { super::dir().join(".sessions.json") }

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

    fn processed(&self, transcript: &Path) -> usize {
        self.transcripts
            .get(&transcript.to_string_lossy().to_string())
            .copied()
            .unwrap_or(0)
    }

    fn record(&mut self, transcript: &Path, lines: usize) {
        self.transcripts
            .insert(transcript.to_string_lossy().to_string(), lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_picks_out_the_labelled_lines() {
        let raw = "\
Here is the record you asked for:

SUMMARY: Wrote the paths module and decided on the registry shape.
DECISION: Markdown stays the source of truth.
- DECISION: Alias the collection instead of re-indexing.
OPEN: migrate still needs a --keep flag.
Some trailing chatter that is not labelled.";

        let d = Digest::parse(raw);
        assert_eq!(d.summary, "Wrote the paths module and decided on the registry shape.");
        assert_eq!(d.decisions.len(), 2);
        assert!(d.decisions[1].contains("Alias the collection"));
        assert_eq!(d.open_threads, vec!["migrate still needs a --keep flag."]);
    }

    #[test]
    fn digest_keeps_the_first_summary_and_tolerates_junk() {
        let d = Digest::parse("SUMMARY: first\nSUMMARY: second\nDECISION:\nOPEN:   \n```");
        assert_eq!(d.summary, "first");
        // Empty labels contribute nothing rather than an empty bullet.
        assert!(d.decisions.is_empty());
        assert!(d.open_threads.is_empty());

        assert_eq!(Digest::parse("no labels at all"), Digest::default());
    }

    #[test]
    fn entry_text_reads_both_message_shapes() {
        let plain = r#"{"message":{"role":"user","content":"hello"}}"#;
        assert_eq!(entry_text(plain).unwrap(), "user: hello");

        let parts = r#"{"message":{"role":"assistant","content":[
            {"type":"text","text":"first"},
            {"type":"tool_use","name":"Bash"},
            {"type":"text","text":"second"}]}}"#;
        assert_eq!(entry_text(parts).unwrap(), "assistant: first\nsecond");

        // Anything without conversational text is skipped.
        assert!(entry_text(r#"{"type":"summary","summary":"x"}"#).is_none());
        assert!(entry_text(r#"{"message":{"role":"system","content":"x"}}"#).is_none());
        assert!(entry_text("not json").is_none());
        assert!(entry_text(r#"{"message":{"role":"user","content":"   "}}"#).is_none());
    }

    #[test]
    fn a_transcript_is_read_from_where_the_last_flush_stopped() {
        let dir = std::env::temp_dir().join(format!("ragpilot-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.jsonl");
        std::fs::write(
            &path,
            "{\"message\":{\"role\":\"user\",\"content\":\"one\"}}\n\
             {\"message\":{\"role\":\"assistant\",\"content\":\"two\"}}\n\
             {\"message\":{\"role\":\"user\",\"content\":\"three\"}}\n",
        )
        .unwrap();

        let (all, total) = read_transcript(&path, 0).unwrap();
        assert_eq!(total, 3);
        assert!(all.contains("one") && all.contains("three"));

        let (rest, total) = read_transcript(&path, 2).unwrap();
        assert_eq!(total, 3);
        assert!(!rest.contains("one"));
        assert!(rest.contains("three"));

        // Nothing new: the guard that stops PreCompact and SessionEnd
        // writing the same block twice.
        let (nothing, _) = read_transcript(&path, 3).unwrap();
        assert!(nothing.trim().is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn truncation_keeps_the_end_of_the_session() {
        let text = (0..500).map(|i| format!("user: turn {i}")).collect::<Vec<_>>().join("\n");
        let out = truncate_tokens(&text, 200);

        assert!(crate::tokens::estimate(&out) <= 200 + crate::tokens::estimate("[earlier turns omitted]\n"));
        assert!(out.contains("turn 499"), "the tail must survive");
        assert!(!out.contains("turn 0\n"), "the head should have been dropped");
        assert!(out.starts_with("[earlier turns omitted]"));

        // Something that already fits is untouched.
        assert_eq!(truncate_tokens("short", 100), "short");
    }
}

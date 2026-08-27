//! `ragpilot brain import` — bring an existing chat archive into the brain.
//!
//! Years of conversations already hold most of what a brain would otherwise
//! spend months learning. Import reads whatever export format is at hand,
//! parks the raw conversations in `archive/takeout/`, and distils them through
//! the same compiler the nightly run uses — same parser, same staging, same
//! never-delete rules.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;

use super::compile::{self, CompileReport};
use super::config::BrainConfig;
use super::{engine, takeout_dir};

/// Progress is reported every this many conversations — a 4,000-conversation
/// archive should not look frozen.
const PROGRESS_EVERY: usize = 10;

// ── the shape everything is parsed into ────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub struct Conversation {
    pub title: String,
    /// ISO date, when the archive records one.
    pub date: Option<String>,
    pub turns: Vec<(String, String)>,
}

impl Conversation {
    /// The conversation as plain text, ready for both the archive and the model.
    fn flatten(&self) -> String {
        self.turns
            .iter()
            .map(|(role, text)| format!("{role}: {text}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn is_empty(&self) -> bool {
        self.turns.is_empty() || self.turns.iter().all(|(_, t)| t.trim().is_empty())
    }
}

/// Archive formats import can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `conversations.json` from a ChatGPT export.
    ChatGpt,
    /// The JSON array a claude.ai export produces.
    ClaudeAi,
    /// One JSON object per line: Claude Code and Codex session logs.
    JsonLines,
    /// A plain markdown or text file.
    PlainText,
}

/// Sniff the format from the content, not the file name — exports get renamed.
pub fn detect(path: &Path, head: &str) -> Option<Format> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    match ext.as_deref() {
        Some("jsonl") => return Some(Format::JsonLines),
        Some("md") | Some("txt") => return Some(Format::PlainText),
        _ => {}
    }

    let trimmed = head.trim_start();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        // ChatGPT nodes hang off a `mapping`; claude.ai keeps a flat
        // `chat_messages` array.
        if head.contains("\"mapping\"") {
            return Some(Format::ChatGpt);
        }
        if head.contains("\"chat_messages\"") {
            return Some(Format::ClaudeAi);
        }
        // A `{`-per-line file is JSON Lines even without the extension.
        if trimmed.starts_with('{') && head.lines().take(3).filter(|l| l.trim().starts_with('{')).count() > 1 {
            return Some(Format::JsonLines);
        }
        return Some(Format::ChatGpt);
    }
    if ext.is_none() { None } else { Some(Format::PlainText) }
}

// ── parsers ────────────────────────────────────────────────────────────────

pub fn parse(format: Format, path: &Path, text: &str) -> Vec<Conversation> {
    match format {
        Format::ChatGpt => parse_chatgpt(text),
        Format::ClaudeAi => parse_claude_ai(text),
        Format::JsonLines => parse_json_lines(path, text),
        Format::PlainText => parse_plain(path, text),
    }
}

/// ChatGPT: an array of conversations, each a `mapping` of message nodes.
fn parse_chatgpt(text: &str) -> Vec<Conversation> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let items = root.as_array().cloned().unwrap_or_else(|| vec![root]);

    items
        .iter()
        .filter_map(|conv| {
            let title = conv.get("title").and_then(|t| t.as_str()).unwrap_or("untitled");
            let date = conv.get("create_time").and_then(epoch_to_date);
            let mapping = conv.get("mapping")?.as_object()?;

            // The mapping is a tree keyed by id; export order is not guaranteed,
            // so order by the message timestamp instead.
            let mut turns: Vec<(f64, String, String)> = mapping
                .values()
                .filter_map(|node| {
                    let message = node.get("message")?;
                    let role = message.pointer("/author/role")?.as_str()?.to_string();
                    if !matches!(role.as_str(), "user" | "assistant") {
                        return None;
                    }
                    let parts = message.pointer("/content/parts")?.as_array()?;
                    let body = parts
                        .iter()
                        .filter_map(|p| p.as_str().map(|s| s.to_string()).or_else(|| {
                            p.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                        }))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let at = message.get("create_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    (!body.trim().is_empty()).then_some((at, role, body))
                })
                .collect();
            turns.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            Some(Conversation {
                title: title.to_string(),
                date,
                turns: turns.into_iter().map(|(_, r, b)| (r, b)).collect(),
            })
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// claude.ai: an array of conversations with a flat `chat_messages` list.
fn parse_claude_ai(text: &str) -> Vec<Conversation> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let items = root.as_array().cloned().unwrap_or_else(|| vec![root]);

    items
        .iter()
        .filter_map(|conv| {
            let title = conv
                .get("name")
                .or_else(|| conv.get("title"))
                .and_then(|t| t.as_str())
                .unwrap_or("untitled");
            let date = conv
                .get("created_at")
                .and_then(|d| d.as_str())
                .map(|d| d.chars().take(10).collect());
            let messages = conv.get("chat_messages")?.as_array()?;

            let turns = messages
                .iter()
                .filter_map(|m| {
                    let role = match m.get("sender").and_then(|s| s.as_str())? {
                        "human" | "user" => "user",
                        "assistant" => "assistant",
                        _ => return None,
                    };
                    let body = m
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            m.get("content")?.as_array().map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                        })?;
                    (!body.trim().is_empty()).then_some((role.to_string(), body))
                })
                .collect();

            Some(Conversation { title: title.to_string(), date, turns })
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// One JSON object per line — Claude Code and Codex session logs. Both are
/// read tolerantly: any line carrying a role and text counts as a turn.
fn parse_json_lines(path: &Path, text: &str) -> Vec<Conversation> {
    let turns: Vec<(String, String)> = text.lines().filter_map(line_turn).collect();
    if turns.is_empty() {
        return Vec::new();
    }
    vec![Conversation {
        title: path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into()),
        date: file_date(path, text),
        turns,
    }]
}

fn line_turn(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    // Claude Code nests under `message`; Codex puts the role at the top level.
    let message = value.get("message").unwrap_or(&value);
    let role = message.get("role").and_then(|r| r.as_str())?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }

    let content = message.get("content")?;
    let body = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                // Claude Code uses `text`; Codex uses `input_text`/`output_text`.
                let kind = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                matches!(kind, "text" | "input_text" | "output_text")
                    .then(|| p.get("text").and_then(|t| t.as_str()))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };

    let body = body.trim();
    (!body.is_empty()).then(|| (role.to_string(), body.to_string()))
}

/// A date for a session log: the file name first (Codex and Claude Code both
/// date theirs), then the first ISO date in the content.
fn file_date(path: &Path, text: &str) -> Option<String> {
    path.file_name()
        .and_then(|n| find_iso_date(&n.to_string_lossy()))
        .or_else(|| find_iso_date(text))
}

/// The first `YYYY-MM-DD` in a string. Scans bytes, so a UTF-8 transcript
/// cannot land it on a character boundary.
fn find_iso_date(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    (0..=bytes.len() - 10).find_map(|i| {
        let window = &bytes[i..i + 10];
        let matches = window[4] == b'-'
            && window[7] == b'-'
            && window
                .iter()
                .enumerate()
                .all(|(j, c)| j == 4 || j == 7 || c.is_ascii_digit());
        matches.then(|| String::from_utf8_lossy(window).to_string())
    })
}

/// A markdown or text file: one document, one "conversation".
fn parse_plain(path: &Path, text: &str) -> Vec<Conversation> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let title = text
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| {
            path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
        });
    vec![Conversation { title, date: file_date(path, text), turns: vec![("document".into(), text.to_string())] }]
}

fn epoch_to_date(value: &serde_json::Value) -> Option<String> {
    let secs = value.as_f64()? as i64;
    chrono::DateTime::from_timestamp(secs, 0).map(|d| d.format("%Y-%m-%d").to_string())
}

// ── the command ────────────────────────────────────────────────────────────

pub struct ImportOptions {
    pub limit: Option<usize>,
    pub since: Option<String>,
    pub engine: Option<String>,
}

pub async fn cmd_import(target: &Path, opts: ImportOptions) -> Result<()> {
    if !super::exists() {
        anyhow::bail!("No brain at {} — run `ragpilot brain init` first.", super::dir().display());
    }
    if !target.exists() {
        anyhow::bail!("{} does not exist.", target.display());
    }

    let files = collect_files(target);
    if files.is_empty() {
        println!("{} Nothing importable at {}.", "i".blue(), target.display());
        return Ok(());
    }

    // Read and parse everything first, so the count in the progress line is
    // real rather than a guess.
    let mut conversations = Vec::new();
    let mut unreadable = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            unreadable.push(file.display().to_string());
            continue;
        };
        let head: String = text.chars().take(4_000).collect();
        match detect(file, &head) {
            Some(format) => conversations.extend(parse(format, file, &text)),
            None => unreadable.push(file.display().to_string()),
        }
    }

    let total_found = conversations.len();
    if let Some(since) = &opts.since {
        conversations.retain(|c| c.date.as_deref().map(|d| d >= since.as_str()).unwrap_or(false));
    }
    // Newest first, so `--limit` keeps the most recent history.
    conversations.sort_by(|a, b| b.date.cmp(&a.date));
    if let Some(limit) = opts.limit {
        conversations.truncate(limit);
    }

    if conversations.is_empty() {
        println!("{} {total_found} conversation(s) found, none matched the filters.", "i".blue());
        return Ok(());
    }
    println!(
        "{} Importing {} of {total_found} conversation(s) from {} file(s)…",
        "→".cyan(),
        conversations.len(),
        files.len()
    );

    let cfg = BrainConfig::load(&super::config_path())?;
    let engine = engine::create(&cfg, opts.engine.as_deref()).map_err(|e| anyhow::anyhow!("{e}"))?;
    engine
        .available()
        .map_err(|e| anyhow::anyhow!("Compiler engine '{}' unavailable: {e}", engine.name()))?;

    std::fs::create_dir_all(takeout_dir())?;
    let distiller = compile::Distiller::new(engine.as_ref());
    let index = distiller.index_header();
    let mut report = CompileReport::default();
    let mut archived = 0usize;

    let count = conversations.len();
    for (i, conv) in conversations.iter().enumerate() {
        let body = conv.flatten();
        archive(conv, &body)?;
        archived += 1;

        let label = format!("{}/{count} “{}”", i + 1, conv.title);
        let input = format!(
            "{index}\n--- conversation: {} ({}) ---\n{}\n",
            conv.title,
            conv.date.as_deref().unwrap_or("no date"),
            body
        );
        distiller.digest(&label, &input, &mut report).await?;

        if (i + 1) % PROGRESS_EVERY == 0 || i + 1 == count {
            println!(
                "  {} {}/{count} · {} note(s) so far",
                "…".dimmed(),
                i + 1,
                report.notes_created.len() + report.notes_updated.len()
            );
        }
    }

    distiller.finish()?;
    report.sources = vec![format!("{archived} conversation(s) from {}", target.display())];
    report.committed = compile::index_and_commit().await;

    compile::print_report(&report);
    if !unreadable.is_empty() {
        println!("  {} {}", "unreadable:".yellow(), unreadable.join(", "));
    }
    println!("  archived to:    {}", takeout_dir().display());
    Ok(())
}

/// Write the raw conversation to the archive. Kept verbatim: the distilled
/// note is an opinion, the archive is the record.
fn archive(conv: &Conversation, body: &str) -> Result<()> {
    let slug = slug_for(conv);
    let path = takeout_dir().join(format!("{slug}.md"));
    if path.exists() {
        return Ok(());
    }
    let text = format!(
        "---\ntitle: {}\ndate: {}\nsource: takeout\nimported: {}\n---\n\n# {}\n\n{}\n",
        conv.title,
        conv.date.as_deref().unwrap_or("unknown"),
        super::vault::today(),
        conv.title,
        body.trim()
    );
    std::fs::write(&path, text).with_context(|| format!("Cannot write {}", path.display()))
}

fn slug_for(conv: &Conversation) -> String {
    let base = conv
        .title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>();
    let base: String = base
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(50)
        .collect();
    let base = if base.is_empty() { "conversation".to_string() } else { base };
    match &conv.date {
        // Session logs are already named for their day; don't say it twice.
        Some(date) if base.starts_with(date.as_str()) => base,
        Some(date) => format!("{date}-{base}"),
        None => base,
    }
}

/// Every candidate file under `target`, recursively.
fn collect_files(target: &Path) -> Vec<PathBuf> {
    if target.is_file() {
        return vec![target.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("json") | Some("jsonl") | Some("md") | Some("txt")
            ) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_archive_format() {
        assert_eq!(
            detect(Path::new("conversations.json"), r#"[{"title":"x","mapping":{}}]"#),
            Some(Format::ChatGpt)
        );
        assert_eq!(
            detect(Path::new("export.json"), r#"[{"name":"x","chat_messages":[]}]"#),
            Some(Format::ClaudeAi)
        );
        assert_eq!(
            detect(Path::new("session.jsonl"), r#"{"message":{"role":"user"}}"#),
            Some(Format::JsonLines)
        );
        // The extension is not trusted: a renamed jsonl is still jsonl.
        assert_eq!(
            detect(Path::new("log"), "{\"role\":\"user\"}\n{\"role\":\"assistant\"}\n"),
            Some(Format::JsonLines)
        );
        assert_eq!(detect(Path::new("notes.md"), "# hi"), Some(Format::PlainText));
    }

    #[test]
    fn chatgpt_turns_come_out_in_time_order() {
        let raw = r#"[{
          "title": "Rust ownership",
          "create_time": 1756300000.0,
          "mapping": {
            "b": {"message": {"author": {"role": "assistant"}, "create_time": 2.0,
                  "content": {"parts": ["Because of the borrow checker."]}}},
            "a": {"message": {"author": {"role": "user"}, "create_time": 1.0,
                  "content": {"parts": ["Why does this not compile?"]}}},
            "sys": {"message": {"author": {"role": "system"}, "create_time": 0.0,
                  "content": {"parts": [""]}}}
          }
        }]"#;

        let convs = parse_chatgpt(raw);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].title, "Rust ownership");
        assert!(convs[0].date.is_some());
        // The mapping is unordered; timestamps decide.
        assert_eq!(convs[0].turns[0].0, "user");
        assert_eq!(convs[0].turns[1].0, "assistant");
        assert_eq!(convs[0].turns.len(), 2, "the system turn should be dropped");
    }

    #[test]
    fn claude_ai_export_is_read() {
        let raw = r#"[{
          "name": "Migration plan",
          "created_at": "2026-08-20T10:00:00Z",
          "chat_messages": [
            {"sender": "human", "text": "How do we migrate?"},
            {"sender": "assistant", "content": [{"type": "text", "text": "Alias the collection."}]},
            {"sender": "system", "text": "ignored"}
          ]
        }]"#;

        let convs = parse_claude_ai(raw);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].date.as_deref(), Some("2026-08-20"));
        assert_eq!(convs[0].turns.len(), 2);
        assert!(convs[0].turns[1].1.contains("Alias the collection"));
    }

    #[test]
    fn json_lines_reads_both_claude_code_and_codex_shapes() {
        let claude_code = "{\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n\
                           {\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n";
        let convs = parse_json_lines(Path::new("2026-08-20-session.jsonl"), claude_code);
        assert_eq!(convs[0].turns.len(), 2);
        assert_eq!(convs[0].date.as_deref(), Some("2026-08-20"));

        let codex = "{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"run the tests\"}]}\n\
                     {\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"all green\"}]}\n\
                     {\"type\":\"function_call\",\"name\":\"shell\"}\n";
        let convs = parse_json_lines(Path::new("codex.jsonl"), codex);
        assert_eq!(convs[0].turns.len(), 2, "tool calls are not turns");
        assert!(convs[0].turns[0].1.contains("run the tests"));
    }

    #[test]
    fn a_plain_document_becomes_one_conversation() {
        let convs = parse_plain(Path::new("notes.md"), "# Deploy runbook\n\nStep one.\n");
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].title, "Deploy runbook");
        assert_eq!(convs[0].turns.len(), 1);

        assert!(parse_plain(Path::new("empty.md"), "   ").is_empty());
    }

    #[test]
    fn a_malformed_archive_yields_nothing_rather_than_panicking() {
        assert!(parse_chatgpt("{ not json").is_empty());
        assert!(parse_claude_ai("[]").is_empty());
        assert!(parse_json_lines(Path::new("x.jsonl"), "garbage\nmore garbage").is_empty());
        // Valid JSON, wrong shape.
        assert!(parse_chatgpt(r#"[{"title":"x"}]"#).is_empty());
    }

    #[test]
    fn archive_slugs_are_dated_and_file_safe() {
        let conv = Conversation {
            title: "Why does this / not compile?!".into(),
            date: Some("2026-08-20".into()),
            turns: vec![],
        };
        assert_eq!(slug_for(&conv), "2026-08-20-why-does-this-not-compile");

        let untitled = Conversation { title: "???".into(), date: None, turns: vec![] };
        assert_eq!(slug_for(&untitled), "conversation");

        let long = Conversation { title: "x".repeat(200), date: None, turns: vec![] };
        assert!(slug_for(&long).len() <= 50);

        // A session log named for its day is not dated twice.
        let session = Conversation {
            title: "2026-06-10-codex".into(),
            date: Some("2026-06-10".into()),
            turns: vec![],
        };
        assert_eq!(slug_for(&session), "2026-06-10-codex");
    }

    #[test]
    fn a_date_comes_from_the_file_name_first_then_the_content() {
        // The name wins — a session file is named for the day it happened.
        assert_eq!(
            file_date(Path::new("2026-08-20-session.jsonl"), "stamped 2026-01-01"),
            Some("2026-08-20".to_string())
        );
        assert_eq!(
            file_date(Path::new("session.jsonl"), "noise 2026-08-20T10:00:00Z more"),
            Some("2026-08-20".to_string())
        );
        assert_eq!(file_date(Path::new("session.jsonl"), "no date here"), None);
    }

    #[test]
    fn find_iso_date_handles_edges_without_panicking() {
        // Exactly ten characters: the boundary the naive range gets wrong.
        assert_eq!(find_iso_date("1234-56-78"), Some("1234-56-78".to_string()));
        assert_eq!(find_iso_date("short"), None);
        assert_eq!(find_iso_date(""), None);
        // Multi-byte characters must not split a slice.
        assert_eq!(find_iso_date("çğüşöı 2026-08-20 ✓"), Some("2026-08-20".to_string()));
        assert_eq!(find_iso_date("çğüşöıçğüşöı"), None);
    }
}

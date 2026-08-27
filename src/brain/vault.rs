//! Reading and writing the vault's markdown.
//!
//! Every write here is an **append**. Nothing in this module edits or deletes
//! an existing line — that guarantee is what lets the compiler (and the agent)
//! be wrong without losing anything.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::{daily_dir, persona_path};
use crate::tokens;

/// Subsection headings written by every flush, so `brain_load` can find them
/// again without guessing. The flush template guarantees the structure; the
/// loader never has to infer it.
pub const DECISIONS_HEADING: &str = "### Decisions";
pub const OPEN_THREADS_HEADING: &str = "### Open threads";

/// How far back `brain_load` looks for decisions.
const RECENT_DAYS: usize = 7;
/// Share of the load budget the persona may take before it is trimmed.
const PERSONA_BUDGET_SHARE: f64 = 0.5;

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn now_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

pub fn daily_path(date: &str) -> PathBuf {
    daily_dir().join(format!("{date}.md"))
}

/// Append to today's daily, creating it with its date heading if needed.
fn append_today(body: &str) -> Result<PathBuf> {
    use std::io::Write;

    let date = today();
    let path = daily_path(&date);
    std::fs::create_dir_all(daily_dir())?;

    let fresh = !path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Cannot open {}", path.display()))?;

    if fresh {
        writeln!(file, "# {date}\n")?;
    }
    write!(file, "{body}")?;
    Ok(path)
}

/// One timestamped line. `kind` is free-form but conventionally one of
/// `note`, `decision`, `task`, `receipt`.
pub fn append_note(kind: &str, text: &str) -> Result<PathBuf> {
    let line = text.trim().replace('\n', " ");
    append_today(&format!("- {} [{kind}] {line}\n", now_hhmm()))
}

/// A session block. The headings are always written — an empty section is a
/// dash, never a missing heading — because `brain_load` reads them back.
pub fn append_flush(summary: &str, decisions: &[String], open_threads: &[String]) -> Result<PathBuf> {
    let block = format!(
        "\n## Session {}\n\n{}\n\n{DECISIONS_HEADING}\n\n{}\n\n{OPEN_THREADS_HEADING}\n\n{}\n",
        now_hhmm(),
        summary.trim(),
        bullets(decisions),
        bullets(open_threads),
    );
    append_today(&block)
}

fn bullets(items: &[String]) -> String {
    let listed: Vec<String> = items
        .iter()
        .map(|i| i.trim())
        .filter(|i| !i.is_empty())
        .map(|i| format!("- {i}"))
        .collect();
    if listed.is_empty() { "—".to_string() } else { listed.join("\n") }
}

// ── The opening package ────────────────────────────────────────────────────

/// The session-opening context: who the agent is, what was left half-done, and
/// what was recently decided.
///
/// The result is **never** larger than `max_tokens`. Sections are added in
/// priority order and the one that does not fit is trimmed, not dropped
/// silently — a truncated section says so.
pub fn load_package(max_tokens: usize) -> Result<String> {
    let persona = std::fs::read_to_string(persona_path()).unwrap_or_default();
    Ok(assemble(&persona, &recent_dailies(RECENT_DAYS), max_tokens))
}

/// The pure core of [`load_package`]: everything it needs is passed in, so the
/// budget guarantee can be tested without a vault on disk.
fn assemble(persona: &str, dailies: &[(String, String)], max_tokens: usize) -> String {
    let mut out = String::new();
    let mut spent = 0usize;

    // One token is held back for the trailing newline the result always ends
    // with, so the total stays inside `max_tokens` and not one over it.
    let max_tokens = max_tokens.saturating_sub(1);
    let persona_budget = (max_tokens as f64 * PERSONA_BUDGET_SHARE) as usize;
    {
        let section = fit(persona.trim(), persona_budget.saturating_sub(spent));
        if !section.is_empty() {
            spent += tokens::estimate(&section);
            out.push_str(&section);
            out.push_str("\n\n");
        }
    }

    if let Some((date, threads)) = latest_section(dailies, OPEN_THREADS_HEADING) {
        let header = format!("## Open threads (from {date})\n\n");
        let budget = max_tokens.saturating_sub(spent + tokens::estimate(&header));
        let body = fit(&threads, budget);
        if !body.is_empty() {
            spent += tokens::estimate(&header) + tokens::estimate(&body);
            out.push_str(&header);
            out.push_str(&body);
            out.push_str("\n\n");
        }
    }

    let decisions = recent_decisions(dailies);
    if !decisions.is_empty() {
        let header = "## Recent decisions\n\n".to_string();
        let budget = max_tokens.saturating_sub(spent + tokens::estimate(&header));
        let body = fit(&decisions.join("\n"), budget);
        if !body.is_empty() {
            out.push_str(&header);
            out.push_str(&body);
            out.push('\n');
        }
    }

    if out.trim().is_empty() {
        // Say which kind of nothing this is: a fresh brain and a budget too
        // small to hold anything are very different problems.
        let note = if persona.trim().is_empty() && dailies.is_empty() {
            "The brain is empty — nothing recorded yet."
        } else {
            "The token budget was too small to include anything from the brain."
        };
        return fit(note, max_tokens);
    }
    out.trim_end().to_string() + "\n"
}

/// Daily files, newest first, at most `count` of them.
fn recent_dailies(count: usize) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(daily_dir()) else { return Vec::new() };

    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    // File names are ISO dates, so lexicographic order is chronological.
    files.sort();
    files.reverse();

    files
        .into_iter()
        .take(count)
        .filter_map(|p| {
            let date = p.file_stem()?.to_string_lossy().to_string();
            let text = std::fs::read_to_string(&p).ok()?;
            Some((date, text))
        })
        .collect()
}

/// The most recent non-empty occurrence of `heading`, with the date it came
/// from. An empty section (the `—` placeholder) is skipped — an agent that
/// closed everything cleanly should not be handed a dash.
fn latest_section(dailies: &[(String, String)], heading: &str) -> Option<(String, String)> {
    for (date, text) in dailies {
        if let Some(body) = last_section_body(text, heading) {
            if !body.trim().is_empty() && body.trim() != "—" {
                return Some((date.clone(), body));
            }
        }
    }
    None
}

/// Body of the *last* `heading` block in a file, up to the next heading.
fn last_section_body(text: &str, heading: &str) -> Option<String> {
    let start = text.rfind(heading)? + heading.len();
    let rest = &text[start..];
    let end = rest
        .find("\n#")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// Decision lines from the recent dailies, newest first: both `[decision]`
/// notes and the bullets under a flush's `### Decisions`.
fn recent_decisions(dailies: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (date, text) in dailies {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.contains("[decision]") {
                out.push(format!("- [{date}] {}", strip_bullet(trimmed)));
            }
        }
        if let Some(body) = last_section_body(text, DECISIONS_HEADING) {
            for line in body.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("- ") && trimmed != "- —" {
                    out.push(format!("- [{date}] {}", strip_bullet(trimmed)));
                }
            }
        }
    }
    out
}

/// Reduce a raw log line to its content: drop the bullet, the `HH:MM` stamp and
/// the `[kind]` marker. The surrounding section already says what these are.
fn strip_bullet(line: &str) -> String {
    let mut rest = line.trim().trim_start_matches("- ").trim();

    if let Some((head, tail)) = rest.split_once(' ') {
        let is_time = head.len() == 5
            && head.as_bytes()[2] == b':'
            && head.chars().enumerate().all(|(i, c)| i == 2 || c.is_ascii_digit());
        if is_time {
            rest = tail.trim();
        }
    }
    if let Some(tail) = rest.strip_prefix('[') {
        if let Some((_, after)) = tail.split_once(']') {
            rest = after.trim();
        }
    }
    rest.to_string()
}

/// Trim `text` to `budget` tokens on a line boundary, marking that it was cut.
/// Returns empty when even the marker would not fit.
fn fit(text: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if tokens::estimate(text) <= budget {
        return text.to_string();
    }

    const MARKER: &str = "\n… (truncated to fit the budget)";
    let marker_cost = tokens::estimate(MARKER);
    if marker_cost >= budget {
        return String::new();
    }
    let room = budget - marker_cost;

    let mut kept = String::new();
    for line in text.lines() {
        let candidate = if kept.is_empty() {
            line.to_string()
        } else {
            format!("{kept}\n{line}")
        };
        if tokens::estimate(&candidate) > room {
            break;
        }
        kept = candidate;
    }
    if kept.is_empty() {
        return String::new();
    }
    format!("{kept}{MARKER}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAILY: &str = "\
# 2026-08-27

- 09:10 [note] warmed up
- 09:30 [decision] markdown stays the source of truth

## Session 11:00

Wrote the paths module.

### Decisions

- alias the collection instead of re-indexing

### Open threads

- migrate needs a --keep flag
- doctor should check the scheduler
";

    #[test]
    fn last_section_body_stops_at_the_next_heading() {
        let body = last_section_body(DAILY, OPEN_THREADS_HEADING).unwrap();
        assert!(body.contains("--keep"));
        assert!(body.contains("scheduler"));
        assert!(!body.contains("Decisions"));

        let body = last_section_body(DAILY, DECISIONS_HEADING).unwrap();
        assert!(body.contains("alias the collection"));
        assert!(!body.contains("Open threads"), "decisions ran into the next section");
    }

    #[test]
    fn decisions_come_from_both_notes_and_flush_bullets() {
        let dailies = vec![("2026-08-27".to_string(), DAILY.to_string())];
        let found = recent_decisions(&dailies);

        assert_eq!(found.len(), 2);
        assert!(found[0].contains("markdown stays the source of truth"));
        assert!(found[1].contains("alias the collection"));
        assert!(found.iter().all(|d| d.contains("[2026-08-27]")));
    }

    #[test]
    fn an_empty_section_is_skipped_not_surfaced() {
        let empty = "# 2026-08-27\n\n### Open threads\n\n—\n";
        let dailies = vec![("2026-08-27".to_string(), empty.to_string())];
        assert!(latest_section(&dailies, OPEN_THREADS_HEADING).is_none());
    }

    #[test]
    fn latest_section_prefers_the_newest_daily() {
        let dailies = vec![
            ("2026-08-27".to_string(), "### Open threads\n\n- newer\n".to_string()),
            ("2026-08-20".to_string(), "### Open threads\n\n- older\n".to_string()),
        ];
        let (date, body) = latest_section(&dailies, OPEN_THREADS_HEADING).unwrap();
        assert_eq!(date, "2026-08-27");
        assert!(body.contains("newer"));
    }

    #[test]
    fn fit_never_exceeds_its_budget() {
        let long = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");

        for budget in [0, 1, 5, 20, 100, 400] {
            let out = fit(&long, budget);
            assert!(
                tokens::estimate(&out) <= budget,
                "budget {budget} exceeded: {} tokens",
                tokens::estimate(&out)
            );
        }
        // Something that already fits is returned untouched.
        assert_eq!(fit("short", 100), "short");
        // A trimmed section says so.
        assert!(fit(&long, 100).contains("truncated"));
    }


    #[test]
    fn strip_bullet_reduces_a_log_line_to_its_content() {
        assert_eq!(strip_bullet("- 09:30 [decision] markdown wins"), "markdown wins");
        assert_eq!(strip_bullet("- alias the collection"), "alias the collection");
        // A bare time-looking word that is not a stamp survives.
        assert_eq!(strip_bullet("- 1:2:3 odd"), "1:2:3 odd");
    }

    #[test]
    fn the_load_package_never_exceeds_its_budget() {
        let persona = (0..80).map(|i| format!("persona line {i}")).collect::<Vec<_>>().join("\n");
        let daily = format!(
            "# 2026-08-27\n\n{}\n\n{OPEN_THREADS_HEADING}\n\n{}\n",
            (0..80).map(|i| format!("- 09:0{} [decision] decision number {i}", i % 10)).collect::<Vec<_>>().join("\n"),
            (0..80).map(|i| format!("- open thread {i}")).collect::<Vec<_>>().join("\n"),
        );
        let dailies = vec![("2026-08-27".to_string(), daily)];

        for budget in [1, 10, 50, 200, 1000, 4000] {
            let out = assemble(&persona, &dailies, budget);
            assert!(
                tokens::estimate(&out) <= budget,
                "budget {budget} exceeded: {} tokens",
                tokens::estimate(&out)
            );
        }
    }

    #[test]
    fn the_load_package_carries_all_three_sections_when_there_is_room() {
        let dailies = vec![("2026-08-27".to_string(), DAILY.to_string())];
        let out = assemble("# Pilot\n\ndirect, concise", &dailies, 4000);

        assert!(out.contains("Pilot"), "persona missing");
        assert!(out.contains("Open threads (from 2026-08-27)"), "open threads missing");
        assert!(out.contains("Recent decisions"), "decisions missing");
        assert!(out.contains("--keep"), "thread content missing");
        assert!(out.contains("markdown stays the source of truth"), "decision content missing");
        // The raw log noise is gone.
        assert!(!out.contains("[decision]"), "kind marker leaked into the package");
    }

    #[test]
    fn an_empty_brain_says_so_instead_of_returning_nothing() {
        let out = assemble("", &[], 1000);
        assert!(out.contains("empty"), "{out}");
    }

    #[test]
    fn bullets_render_an_empty_list_as_a_dash() {
        assert_eq!(bullets(&[]), "—");
        assert_eq!(bullets(&["  ".to_string()]), "—");
        assert_eq!(bullets(&["a".to_string(), "b".to_string()]), "- a\n- b");
    }
}

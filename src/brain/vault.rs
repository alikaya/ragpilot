//! Reading and writing the vault's markdown.
//!
//! Every write here is an **append**. Nothing in this module edits or deletes
//! an existing line — that guarantee is what lets the compiler (and the agent)
//! be wrong without losing anything.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::{daily_dir, dir, persona_path};
use crate::tokens;

/// Subsection headings written by every flush, so `brain_load` can find them
/// again without guessing. The flush template guarantees the structure; the
/// loader never has to infer it.
pub const DECISIONS_HEADING: &str = "### Decisions";
pub const OPEN_THREADS_HEADING: &str = "### Open threads";

/// How far back `brain_load` looks for decisions.
const RECENT_DAYS: usize = 7;
/// Share of the load budget the persona may take before it is trimmed.
const PERSONA_BUDGET_SHARE: f64 = 0.4;
/// Share reserved for rules. They are instructions, not background — trimming
/// them away silently is how the same correction gets made twice.
const RULES_BUDGET_SHARE: f64 = 0.3;

/// Corrections the user has given, kept until they are edited out by hand.
pub fn rules_path() -> PathBuf { dir().join("rules.md") }
/// Work that is open across days, rather than only in the last session.
pub fn threads_path() -> PathBuf { dir().join("threads.md") }

const ACTIVE_HEADING: &str = "## Active";
const CLOSED_HEADING: &str = "## Closed";

/// Starting content for the two files `brain init` creates.
pub fn rules_template(today: &str) -> String {
    format!(
        "---\ntitle: Rules\nupdated: {today}\n---\n\n\
         # Rules\n\n\
         Corrections you were given, and the reason behind each one. Every session\n\
         starts with this file, so a correction only has to be made once.\n\n\
         Add one with `brain_note` and `kind: \"rule\"`. Edit or delete freely — this\n\
         file is yours; the compiler never rewrites it.\n"
    )
}

pub fn threads_template(today: &str) -> String {
    format!(
        "---\ntitle: Threads\nupdated: {today}\n---\n\n\
         # Threads\n\n\
         Work that is still open, carried across sessions until it is closed.\n\
         `brain_flush` adds and closes items here.\n\n\
         {ACTIVE_HEADING}\n\n{CLOSED_HEADING}\n"
    )
}

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

/// Record a correction as a standing rule. Appended, never rewritten, and
/// skipped when the same rule is already there — an agent told the same thing
/// twice should not produce two rules.
pub fn append_rule(rule: &str, reason: Option<&str>) -> Result<PathBuf> {
    let path = rules_path();
    let mut text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| rules_template(&today()));

    let rule = rule.trim().replace('\n', " ");
    if text.contains(&rule) {
        return Ok(path);
    }
    let entry = match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(why) => format!("\n- **rule:** {rule}\n  **why:** {}\n", why.replace('\n', " ")),
        None => format!("\n- **rule:** {rule}\n"),
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&entry);
    std::fs::write(&path, text).with_context(|| format!("Cannot write {}", path.display()))?;
    Ok(path)
}

// ── threads ────────────────────────────────────────────────────────────────

/// Open new threads and close finished ones.
///
/// Open work survives until it is closed, rather than only until the next
/// session writes a different list — a thread nobody mentioned for three days
/// is exactly the one worth being reminded of.
pub fn update_threads(open: &[String], closed: &[String]) -> Result<PathBuf> {
    let path = threads_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| threads_template(&today()));
    let (mut active, mut done) = split_threads(&text);
    let today = today();

    for item in closed {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        // Closing something matches loosely: the agent rarely quotes its own
        // earlier wording exactly.
        let hit = active.iter().position(|a| similar(a, item));
        let line = match hit {
            Some(i) => active.remove(i),
            None => item.to_string(),
        };
        let line = strip_opened(&line);
        if !done.iter().any(|d| similar(d, &line)) {
            done.push(format!("{line} (closed {today})"));
        }
    }

    for item in open {
        let item = item.trim();
        if item.is_empty() || active.iter().any(|a| similar(a, item)) {
            continue;
        }
        active.push(format!("{item} (opened {today})"));
    }

    let out = format!(
        "---\ntitle: Threads\nupdated: {today}\n---\n\n# Threads\n\n\
         Work that is still open, carried across sessions until it is closed.\n\
         `brain_flush` adds and closes items here.\n\n\
         {ACTIVE_HEADING}\n\n{}\n\n{CLOSED_HEADING}\n\n{}\n",
        render_threads(&active),
        render_threads(&done),
    );
    std::fs::write(&path, out).with_context(|| format!("Cannot write {}", path.display()))?;
    Ok(path)
}

fn render_threads(items: &[String]) -> String {
    if items.is_empty() {
        "—".to_string()
    } else {
        items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n")
    }
}

fn split_threads(text: &str) -> (Vec<String>, Vec<String>) {
    let body = |heading: &str| -> Vec<String> {
        last_section_body(text, heading)
            .map(|b| {
                b.lines()
                    .map(|l| l.trim())
                    .filter(|l| l.starts_with("- ") && *l != "- —")
                    .map(|l| l.trim_start_matches("- ").trim().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    (body(ACTIVE_HEADING), body(CLOSED_HEADING))
}

fn strip_opened(line: &str) -> String {
    match line.rfind(" (opened ") {
        Some(i) if line.ends_with(')') => line[..i].to_string(),
        _ => line.to_string(),
    }
}

/// Loose match: same text ignoring case, punctuation and the trailing stamp.
fn similar(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        strip_opened(s)
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

/// Open threads recorded in the dailies but not yet in `threads.md`.
///
/// An upgraded vault has months of open work living only in its last session
/// block. Creating an empty `threads.md` beside it would drop all of it, since
/// the loader prefers the standing list once it has anything in it.
pub fn threads_from_dailies() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, text) in recent_dailies(RECENT_DAYS) {
        for body in section_bodies(&text, OPEN_THREADS_HEADING) {
            for line in body.lines() {
                let line = line.trim();
                if !line.starts_with("- ") || line == "- —" {
                    continue;
                }
                let item = line.trim_start_matches("- ").trim().to_string();
                if !out.iter().any(|existing| similar(existing, &item)) {
                    out.push(item);
                }
            }
        }
    }
    out
}

/// Active threads, as the loader wants them.
pub fn active_threads() -> Vec<String> {
    std::fs::read_to_string(threads_path())
        .map(|t| split_threads(&t).0)
        .unwrap_or_default()
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
    let rules = std::fs::read_to_string(rules_path()).unwrap_or_default();
    let dailies = recent_dailies(RECENT_DAYS);

    // Threads first from the standing list; a vault that predates it falls back
    // to the last session's block so nothing is lost on upgrade.
    let threads = match active_threads() {
        items if !items.is_empty() => Some((
            "still open".to_string(),
            items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"),
        )),
        _ => latest_section(&dailies, OPEN_THREADS_HEADING).map(|(d, b)| (format!("from {d}"), b)),
    };

    Ok(assemble(&persona, &rules, threads.as_ref(), &recent_decisions(&dailies), max_tokens))
}

/// The pure core of [`load_package`]: everything it needs is passed in, so the
/// budget guarantee can be tested without a vault on disk.
fn assemble(
    persona: &str,
    rules: &str,
    threads: Option<&(String, String)>,
    decisions: &[String],
    max_tokens: usize,
) -> String {
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

    // Rules come before anything situational: they are how the agent is asked
    // to behave, not background it may skim.
    {
        let body = rules_entries(rules);
        if !body.is_empty() {
            let header = "## Rules\n\n".to_string();
            let budget = ((max_tokens as f64 * RULES_BUDGET_SHARE) as usize)
                .min(max_tokens.saturating_sub(spent + tokens::estimate(&header)));
            let body = fit(&body, budget);
            if !body.is_empty() {
                spent += tokens::estimate(&header) + tokens::estimate(&body);
                out.push_str(&header);
                out.push_str(&body);
                out.push_str("\n\n");
            }
        }
    }

    if let Some((source, threads)) = threads {
        let header = format!("## Open threads ({source})\n\n");
        let budget = max_tokens.saturating_sub(spent + tokens::estimate(&header));
        let body = fit(threads, budget);
        if !body.is_empty() {
            spent += tokens::estimate(&header) + tokens::estimate(&body);
            out.push_str(&header);
            out.push_str(&body);
            out.push_str("\n\n");
        }
    }

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
        let note = if persona.trim().is_empty() && decisions.is_empty() && threads.is_none() {
            "The brain is empty — nothing recorded yet."
        } else {
            "The token budget was too small to include anything from the brain."
        };
        return fit(note, max_tokens);
    }
    out.trim_end().to_string() + "\n"
}

/// Just the rule bullets from `rules.md` — the heading and the how-to prose
/// around them are for a human reading the file, not for the context window.
fn rules_entries(text: &str) -> String {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| l.trim_start().starts_with("- **rule:**") || l.trim_start().starts_with("**why:**"))
        .collect();
    kept.join("\n")
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

/// Bodies of *every* `heading` block in a file. Seeding an upgraded vault has
/// to see all of a day's sessions, not just the last one.
fn section_bodies(text: &str, heading: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(heading) {
        let start = from + rel + heading.len();
        let rest = &text[start..];
        let end = rest.find("\n#").map(|i| i + 1).unwrap_or(rest.len());
        out.push(rest[..end].trim().to_string());
        from = start + end;
    }
    out
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

        let threads = ("still open".to_string(), (0..80).map(|i| format!("- open thread {i}")).collect::<Vec<_>>().join("\n"));
        let rules = (0..40)
            .map(|i| format!("- **rule:** rule number {i}\n  **why:** because {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let decisions = recent_decisions(&dailies);

        for budget in [1, 10, 50, 200, 1000, 4000] {
            let out = assemble(&persona, &rules, Some(&threads), &decisions, budget);
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
        let threads = latest_section(&dailies, OPEN_THREADS_HEADING)
            .map(|(d, b)| (format!("from {d}"), b))
            .unwrap();
        let rules = "- **rule:** answer first, warm up never\n  **why:** Ali says so";
        let out = assemble(
            "# Pilot\n\ndirect, concise",
            rules,
            Some(&threads),
            &recent_decisions(&dailies),
            4000,
        );

        assert!(out.contains("Pilot"), "persona missing");
        assert!(out.contains("## Rules"), "rules missing");
        assert!(out.contains("answer first"), "rule text missing");
        assert!(out.contains("Open threads (from 2026-08-27)"), "open threads missing");
        assert!(out.contains("Recent decisions"), "decisions missing");
        assert!(out.contains("--keep"), "thread content missing");
        assert!(out.contains("markdown stays the source of truth"), "decision content missing");
        // The raw log noise is gone.
        assert!(!out.contains("[decision]"), "kind marker leaked into the package");
    }

    #[test]
    fn an_empty_brain_says_so_instead_of_returning_nothing() {
        let out = assemble("", "", None, &[], 1000);
        assert!(out.contains("empty"), "{out}");
    }

    #[test]
    fn bullets_render_an_empty_list_as_a_dash() {
        assert_eq!(bullets(&[]), "—");
        assert_eq!(bullets(&["  ".to_string()]), "—");
        assert_eq!(bullets(&["a".to_string(), "b".to_string()]), "- a\n- b");
    }
}

#[cfg(test)]
mod rules_and_threads_tests {
    use super::*;

    #[test]
    fn only_the_rule_bullets_reach_the_context_window() {
        let file = format!(
            "{}\n- **rule:** answer first\n  **why:** no warm-up paragraphs\n- **rule:** read before writing\n",
            rules_template("2026-08-27")
        );
        let entries = rules_entries(&file);

        assert!(entries.contains("answer first"));
        assert!(entries.contains("no warm-up"));
        assert!(entries.contains("read before writing"));
        // The prose that explains the file to a human is not context.
        assert!(!entries.contains("Corrections you were given"));
        assert!(!entries.contains("# Rules"));
        assert!(!entries.contains("title: Rules"));

        assert_eq!(rules_entries(&rules_template("2026-08-27")), "");
    }

    #[test]
    fn threads_split_into_active_and_closed() {
        let text = format!(
            "# Threads\n\n{ACTIVE_HEADING}\n\n- one (opened 2026-08-01)\n- two\n\n{CLOSED_HEADING}\n\n- three (closed 2026-08-02)\n"
        );
        let (active, closed) = split_threads(&text);
        assert_eq!(active, vec!["one (opened 2026-08-01)", "two"]);
        assert_eq!(closed, vec!["three (closed 2026-08-02)"]);

        // The dash placeholder is not a thread.
        let empty = format!("{ACTIVE_HEADING}\n\n—\n\n{CLOSED_HEADING}\n\n—\n");
        assert_eq!(split_threads(&empty), (vec![], vec![]));
    }

    #[test]
    fn similar_matches_loosely_but_not_wildly() {
        assert!(similar("migrate needs a --keep flag", "Migrate needs a --keep flag."));
        assert!(similar("doctor should check the scheduler (opened 2026-08-01)", "doctor should check the scheduler"));
        // A substring is close enough — the agent rarely quotes itself exactly.
        assert!(similar("finish the migration docs", "migration docs"));
        // Unrelated work is not.
        assert!(!similar("write the release notes", "fix the parser"));
        assert!(!similar("", "anything"));
    }

    #[test]
    fn strip_opened_removes_only_a_trailing_stamp() {
        assert_eq!(strip_opened("a thing (opened 2026-08-01)"), "a thing");
        assert_eq!(strip_opened("a thing"), "a thing");
        // Not a stamp: left alone.
        assert_eq!(strip_opened("a thing (opened by hand) and more"), "a thing (opened by hand) and more");
    }
}

#[cfg(test)]
mod seeding_tests {
    use super::*;

    #[test]
    fn seeding_sees_every_session_block_not_just_the_last() {
        let day = format!(
            "# 2026-08-27\n\n## Session 10:00\n\nfirst\n\n{OPEN_THREADS_HEADING}\n\n- alpha\n- beta\n\n\
             ## Session 18:00\n\nsecond\n\n{OPEN_THREADS_HEADING}\n\n- gamma\n"
        );
        let bodies = section_bodies(&day, OPEN_THREADS_HEADING);
        assert_eq!(bodies.len(), 2, "only one block was seen");
        assert!(bodies[0].contains("alpha") && bodies[0].contains("beta"));
        assert!(bodies[1].contains("gamma"));
        // …and a block never runs into the next heading.
        assert!(!bodies[0].contains("gamma"));

        // The loader's own fallback still wants the newest block only.
        let latest = last_section_body(&day, OPEN_THREADS_HEADING).unwrap();
        assert!(latest.contains("gamma") && !latest.contains("alpha"));
    }
}

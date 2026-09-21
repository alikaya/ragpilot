//! Interactive `init` configuration: detect the project's languages and
//! source directories, present them as pre-checked defaults, and let the user
//! confirm or adjust. Falls back to pure auto-detection when stdin/stdout is
//! not a TTY (scripts, agent-driven `setup`), so it never blocks.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use colored::Colorize;
use walkdir::WalkDir;

/// (display name, canonical file extensions)
const LANGUAGES: &[(&str, &[&str])] = &[
    ("rust",        &["rs"]),
    ("python",      &["py", "pyi"]),
    ("javascript",  &["js", "jsx", "mjs", "cjs"]),
    ("typescript",  &["ts", "tsx"]),
    ("go",          &["go"]),
    ("java",        &["java"]),
    ("kotlin",      &["kt", "kts"]),
    ("c",           &["c", "h"]),
    ("cpp",         &["cpp", "cc", "cxx", "hpp", "hh"]),
    ("csharp",      &["cs"]),
    ("ruby",        &["rb"]),
    ("php",         &["php"]),
    ("swift",       &["swift"]),
    ("lua",         &["lua"]),
    ("godot",       &["gd", "gdshader", "gdshaderinc"]),
    ("qml",         &["qml"]),
    ("dart/flutter", &["dart"]),
    ("web",         &["html", "css", "scss", "vue", "svelte"]),
    ("shell",       &["sh", "bash"]),
    ("docs/config", &["md", "json", "yaml", "yml", "toml"]),
];

/// Directories commonly holding source, offered for `include_dirs`.
const CANDIDATE_DIRS: &[&str] = &[
    "src", "app", "lib", "pkg", "cmd", "internal",
    "packages", "source", "components", "server", "client",
];

/// Candidate dirs narrow the index only when they hold at least this share of
/// the project's code. Below it, the code lives under names the list does not
/// know — `modules/`, `services/`, one folder per feature — and restricting to
/// the dirs that happen to match would index a fraction of the project with
/// nothing to say so.
const DIR_COVERAGE: f64 = 0.9;

/// Directories never descended into during detection (config does not exist yet).
const SCAN_EXCLUDE: &[&str] = &[
    ".git", ".rag", ".codex", ".fastembed_cache", "target",
    "node_modules", "__pycache__", ".venv", "venv", "dist", "build",
    ".next", ".nuxt", "vendor", "coverage", ".cache", ".turbo",
    ".idea", ".vscode",
];

pub struct IndexChoices {
    pub extensions:   Vec<String>,
    pub include_dirs: Vec<String>,
}

/// Detect languages/dirs, then either prompt (TTY) or auto-pick (non-TTY).
pub fn configure(root: &Path) -> IndexChoices {
    let (ext_counts, detected_dirs, dirs_cover_code) = detect(root);
    let lang_detected: Vec<bool> = LANGUAGES
        .iter()
        .map(|(_, exts)| exts.iter().any(|e| ext_counts.contains_key(*e)))
        .collect();

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if !interactive {
        let dirs: &[String] = if dirs_cover_code { &detected_dirs } else { &[] };
        return auto_choices(&ext_counts, &lang_detected, dirs);
    }

    let extensions   = prompt_languages(&ext_counts, &lang_detected);
    let include_dirs = prompt_dirs(&detected_dirs, dirs_cover_code);
    IndexChoices { extensions, include_dirs }
}

/// Walk the project (depth-limited, excluding caches/vendored dirs) and tally
/// file extensions; also report which well-known source dirs exist and
/// whether they hold enough of the code to index them alone.
fn detect(root: &Path) -> (BTreeMap<String, usize>, Vec<String>, bool) {
    let mut ext_counts: BTreeMap<String, usize> = BTreeMap::new();
    // Code files, overall and under a candidate dir. Docs and config do not
    // count: a README at the root is not a sign the sources live elsewhere.
    let code_exts: Vec<&str> = LANGUAGES
        .iter()
        .filter(|(name, _)| *name != "docs/config")
        .flat_map(|(_, exts)| exts.iter().copied())
        .collect();
    let (mut code_total, mut code_in_candidates) = (0usize, 0usize);

    for entry in WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !SCAN_EXCLUDE.iter().any(|d| name == *d);
            }
            true
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(ext) = entry.path().extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if code_exts.contains(&ext.as_str()) {
                code_total += 1;
                let top = entry.path().strip_prefix(root).ok()
                    .and_then(|rel| rel.components().next())
                    .map(|c| c.as_os_str().to_string_lossy().to_string());
                if top.is_some_and(|t| CANDIDATE_DIRS.contains(&t.as_str()) && entry.depth() > 1) {
                    code_in_candidates += 1;
                }
            }
            *ext_counts.entry(ext).or_insert(0) += 1;
        }
    }

    let dirs: Vec<String> = CANDIDATE_DIRS
        .iter()
        .filter(|d| root.join(d).is_dir())
        .map(|d| d.to_string())
        .collect();

    let covers = code_total == 0 || code_in_candidates as f64 >= code_total as f64 * DIR_COVERAGE;
    (ext_counts, dirs, covers)
}

fn prompt_languages(ext_counts: &BTreeMap<String, usize>, detected: &[bool]) -> Vec<String> {
    println!("\n{}", "Languages to index:".bold());
    for (i, (name, exts)) in LANGUAGES.iter().enumerate() {
        let mark = if detected[i] { "✓".green() } else { " ".normal() };
        let n_files: usize = exts
            .iter()
            .map(|e| ext_counts.get(*e).copied().unwrap_or(0))
            .sum();
        let hint = if n_files > 0 {
            format!(" ({n_files} files)")
        } else {
            String::new()
        };
        println!(
            "  [{:>2}] {} {:<11} {}{}",
            i + 1,
            mark,
            name,
            exts.join(", ").dimmed(),
            hint.dimmed()
        );
    }

    let default_langs: Vec<usize> = (0..LANGUAGES.len()).filter(|&i| detected[i]).collect();
    let def_str = numbers(&default_langs);
    let input = read_line(&format!(
        "Selection (space-separated numbers) [default: {}]: ",
        if def_str.is_empty() { "—".into() } else { def_str }
    ));

    let chosen = if input.is_empty() {
        default_langs
    } else {
        parse_selection(&input, LANGUAGES.len())
    };

    let mut extensions = exts_from_langs(&chosen);
    if extensions.is_empty() {
        // user cleared everything and nothing was detected — keep docs at least
        extensions.push("md".to_string());
    }
    extensions
}

fn prompt_dirs(detected: &[String], cover_code: bool) -> Vec<String> {
    println!("\n{}", "Directories to index:".bold());
    println!("  [ 1] {}", "(entire project root)".dimmed());
    for (i, d) in detected.iter().enumerate() {
        println!("  [{:>2}] {}/", i + 2, d);
    }

    // default: the detected dirs when they hold the code, else the whole root
    let default_sel: Vec<usize> = if detected.is_empty() || !cover_code {
        vec![0]
    } else {
        (1..=detected.len()).collect()
    };
    let input = read_line(&format!(
        "Selection [default: {}]: ",
        numbers(&default_sel)
    ));

    let sel = if input.is_empty() {
        default_sel
    } else {
        parse_selection(&input, detected.len() + 1)
    };

    // option index 0 == "whole root" → empty include_dirs
    if sel.is_empty() || sel.contains(&0) {
        return Vec::new();
    }
    sel.iter()
        .filter(|&&i| i >= 1)
        .map(|&i| detected[i - 1].clone())
        .collect()
}

/// Non-TTY: pick every detected language's extensions that actually occur,
/// plus docs; dirs = detected source dirs (empty → whole root).
fn auto_choices(
    ext_counts: &BTreeMap<String, usize>,
    lang_detected: &[bool],
    detected_dirs: &[String],
) -> IndexChoices {
    let chosen: Vec<usize> = (0..LANGUAGES.len()).filter(|&i| lang_detected[i]).collect();
    let mut extensions: Vec<String> = exts_from_langs(&chosen)
        .into_iter()
        .filter(|e| ext_counts.contains_key(e))
        .collect();

    if extensions.is_empty() {
        eprintln!(
            "{}",
            "ragpilot: no known source files detected; defaulting to rs/toml/md."
                .yellow()
        );
        extensions = vec!["rs".into(), "toml".into(), "md".into()];
    }
    if !extensions.iter().any(|e| e == "md") {
        extensions.push("md".into());
    }

    IndexChoices {
        extensions,
        include_dirs: detected_dirs.to_vec(),
    }
}

fn exts_from_langs(chosen: &[usize]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &i in chosen {
        if let Some((_, exts)) = LANGUAGES.get(i) {
            for e in *exts {
                if !out.iter().any(|x| x == e) {
                    out.push(e.to_string());
                }
            }
        }
    }
    out
}

fn read_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim().to_string()
}

/// Parse "1 3, 5" into zero-based indices, keeping only values in `1..=n`.
fn parse_selection(input: &str, n: usize) -> Vec<usize> {
    let mut out: Vec<usize> = input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<usize>().ok())
        .filter(|&i| i >= 1 && i <= n)
        .map(|i| i - 1)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Render zero-based indices back as 1-based, space-separated.
fn numbers(idx: &[usize]) -> String {
    idx.iter()
        .map(|i| (i + 1).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::detect;
    use std::fs;

    fn project(name: &str, files: &[&str]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("ragpilot_wizard_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for f in files {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        root
    }

    #[test]
    fn candidate_dirs_holding_the_code_narrow_the_index() {
        let root = project("src", &["src/main.rs", "src/lib.rs", "src/a/b.rs", "README.md", "Cargo.toml"]);
        let (_, dirs, covers) = detect(&root);
        assert_eq!(dirs, vec!["src".to_string()]);
        assert!(covers, "docs and config at the root must not count against src");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_candidate_dir_holding_a_fraction_of_the_code_does_not() {
        // A Quickshell-style layout: only components/ is a known name.
        let root = project("qml", &["shell.qml", "components/Button.qml",
            "modules/bar/Bar.qml", "modules/bar/Clock.qml", "services/Audio.qml"]);
        let (_, dirs, covers) = detect(&root);
        assert_eq!(dirs, vec!["components".to_string()]);
        assert!(!covers, "1 of 5 files is in components/; indexing it alone loses the rest");
        fs::remove_dir_all(&root).ok();
    }
}

use regex::Regex;
use std::sync::OnceLock;

use super::{CallRef, Import, ParsedFile, Parser, Symbol};

pub struct RegexParser;

impl Parser for RegexParser {
    fn parse(&self, path: &str, content: &str, language: &str) -> ParsedFile {
        let mut parsed = ParsedFile {
            path: path.to_string(),
            ..Default::default()
        };

        parsed.symbols = extract_symbols(path, content, language);
        parsed.imports = extract_imports(path, content, language);
        parsed.calls   = extract_calls(path, content, &parsed.symbols);

        parsed
    }
}

// ─── Symbol extraction ───────────────────────────────────────────────────────

fn extract_symbols(path: &str, content: &str, language: &str) -> Vec<Symbol> {
    let patterns = symbol_patterns(language);
    let mut symbols = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        for (prefix, kind) in &patterns {
            if let Some(rest) = trimmed.strip_prefix(prefix.as_str()) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name.len() >= 2 {
                    let id = format!("{}::{}", path, name);
                    symbols.push(Symbol {
                        id,
                        path: path.to_string(),
                        name,
                        kind: kind.to_string(),
                        start_line: line_no + 1,
                        end_line:   line_no + 1, // estimated; refined below
                    });
                }
            }
        }
    }

    // Estimate end_lines: next symbol's start - 1
    let starts: Vec<usize> = symbols.iter().map(|s| s.start_line).collect();
    let total_lines = content.lines().count();
    for (i, sym) in symbols.iter_mut().enumerate() {
        sym.end_line = if i + 1 < starts.len() {
            starts[i + 1].saturating_sub(1).max(sym.start_line)
        } else {
            total_lines
        };
    }

    symbols
}

fn symbol_patterns(language: &str) -> Vec<(String, &'static str)> {
    match language {
        "rust" => vec![
            ("pub async fn ".into(), "function"),
            ("pub fn ".into(), "function"),
            ("async fn ".into(), "function"),
            ("fn ".into(), "function"),
            ("pub struct ".into(), "struct"),
            ("struct ".into(), "struct"),
            ("pub enum ".into(), "enum"),
            ("enum ".into(), "enum"),
            ("pub trait ".into(), "trait"),
            ("trait ".into(), "trait"),
            ("impl ".into(), "impl"),
        ],
        "python" => vec![
            ("async def ".into(), "function"),
            ("def ".into(), "function"),
            ("class ".into(), "class"),
        ],
        "javascript" | "typescript" => vec![
            ("export async function ".into(), "function"),
            ("export function ".into(), "function"),
            ("async function ".into(), "function"),
            ("function ".into(), "function"),
            ("export class ".into(), "class"),
            ("class ".into(), "class"),
            ("export const ".into(), "const"),
            ("const ".into(), "const"),
        ],
        "go" => vec![
            ("func ".into(), "function"),
            ("type ".into(), "type"),
        ],
        "java" | "kotlin" | "scala" => vec![
            ("public class ".into(), "class"),
            ("class ".into(), "class"),
            ("public interface ".into(), "interface"),
            ("interface ".into(), "interface"),
            ("fun ".into(), "function"),
            ("public ".into(), "function"),
        ],
        "ruby" => vec![
            ("def ".into(), "function"),
            ("class ".into(), "class"),
            ("module ".into(), "module"),
        ],
        "php" => vec![
            ("function ".into(), "function"),
            ("class ".into(), "class"),
        ],
        "swift" => vec![
            ("func ".into(), "function"),
            ("class ".into(), "class"),
            ("struct ".into(), "struct"),
        ],
        _ => vec![],
    }
}

// ─── Import extraction ───────────────────────────────────────────────────────

fn extract_imports(path: &str, content: &str, language: &str) -> Vec<Import> {
    match language {
        "rust"       => extract_rust_imports(path, content),
        "python"     => extract_python_imports(path, content),
        "javascript" | "typescript" => extract_js_imports(path, content),
        "go"         => extract_go_imports(path, content),
        _            => vec![],
    }
}

fn extract_rust_imports(path: &str, content: &str) -> Vec<Import> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^use\s+([\w:]+(?:::\{[^}]+\}|::\*|\w+)?)\s*;").unwrap());
    content.lines().filter_map(|line| {
        let t = line.trim();
        re.captures(t).map(|c| {
            let full = c[1].to_string();
            let last = full.split("::").last().unwrap_or("*").to_string();
            Import { importer: path.to_string(), from_module: full, symbol_name: last }
        })
    }).collect()
}

fn extract_python_imports(path: &str, content: &str) -> Vec<Import> {
    static FROM_RE: OnceLock<Regex> = OnceLock::new();
    static IMPORT_RE: OnceLock<Regex> = OnceLock::new();
    let from_re   = FROM_RE.get_or_init(||   Regex::new(r"^from\s+(\S+)\s+import\s+(.+)").unwrap());
    let import_re = IMPORT_RE.get_or_init(|| Regex::new(r"^import\s+(\S+)").unwrap());

    content.lines().flat_map(|line| {
        let t = line.trim();
        if let Some(c) = from_re.captures(t) {
            let module = c[1].to_string();
            c[2].split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(|sym| Import { importer: path.to_string(), from_module: module.clone(), symbol_name: sym })
                .collect::<Vec<_>>()
        } else if let Some(c) = import_re.captures(t) {
            vec![Import { importer: path.to_string(), from_module: c[1].to_string(), symbol_name: "*".into() }]
        } else {
            vec![]
        }
    }).collect()
}

fn extract_js_imports(path: &str, content: &str) -> Vec<Import> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"import\s+(?:\{([^}]+)\}|(\w+))\s+from\s+['"]([^'"]+)['"]"#).unwrap());
    content.lines().flat_map(|line| {
        let t = line.trim();
        if let Some(c) = re.captures(t) {
            let module = c[3].to_string();
            let symbols = c.get(1)
                .map(|m| m.as_str().split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>())
                .or_else(|| c.get(2).map(|m| vec![m.as_str().to_string()]))
                .unwrap_or_else(|| vec!["*".into()]);
            symbols.into_iter().map(|sym| Import {
                importer: path.to_string(), from_module: module.clone(), symbol_name: sym,
            }).collect()
        } else {
            vec![]
        }
    }).collect()
}

fn extract_go_imports(path: &str, content: &str) -> Vec<Import> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"^\s*"([^"]+)""#).unwrap());
    let mut in_import_block = false;
    let mut imports = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t == "import (" { in_import_block = true; continue; }
        if in_import_block && t == ")" { in_import_block = false; continue; }
        if in_import_block || t.starts_with("import \"") {
            if let Some(c) = re.captures(t) {
                imports.push(Import {
                    importer: path.to_string(),
                    from_module: c[1].to_string(),
                    symbol_name: "*".into(),
                });
            }
        }
    }
    imports
}

// ─── Call detection ──────────────────────────────────────────────────────────

/// Detect calls by looking for `symbolName(` patterns in the file,
/// attributed to the most-recently-seen caller symbol.
fn extract_calls(path: &str, content: &str, symbols: &[Symbol]) -> Vec<CallRef> {
    if symbols.is_empty() {
        return vec![];
    }

    // Build a sorted list of (start_line, symbol_id) to find which symbol
    // "owns" each line.
    let mut ranges: Vec<(usize, usize, &str)> = symbols.iter()
        .map(|s| (s.start_line, s.end_line, s.id.as_str()))
        .collect();
    ranges.sort_by_key(|r| r.0);

    // Known symbol names (deduplicated)
    let known_names: std::collections::HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

    static CALL_RE: OnceLock<Regex> = OnceLock::new();
    let call_re = CALL_RE.get_or_init(|| Regex::new(r"\b(\w{2,})\s*\(").unwrap());

    let mut calls = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        let lno = line_no + 1;
        // Find which caller symbol owns this line
        let caller_id = ranges.iter().rev()
            .find(|(s, e, _)| lno >= *s && lno <= *e)
            .map(|(_, _, id)| *id);

        if let Some(caller) = caller_id {
            for cap in call_re.captures_iter(line) {
                let callee = &cap[1];
                if known_names.contains(callee) && !caller.ends_with(&format!("::{}", callee)) {
                    calls.push(CallRef {
                        caller_id:   caller.to_string(),
                        callee_name: callee.to_string(),
                        call_line:   lno,
                    });
                }
            }
        }
    }

    calls.dedup_by(|a, b| a.caller_id == b.caller_id && a.callee_name == b.callee_name);
    calls
}

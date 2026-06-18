//! AST-style "context pruning": render a file's *skeleton* — signatures,
//! type/struct/enum definitions, imports and doc comments — with function
//! and method bodies elided to `...`. This preserves the structure an LLM
//! needs to navigate code while cutting the bulk of the tokens.
//!
//! The project uses a regex symbol parser (not a full AST), so this is a
//! pragmatic, best-effort skeletonizer: brace languages are reduced by
//! balanced-brace matching, Python by indentation. Unsupported languages
//! are returned unchanged (safe no-op).

use crate::parser::{Parser, RegexParser};

/// Produce a skeleton view of `content` for the given `language`.
pub fn skeletonize(content: &str, language: &str) -> String {
    match language {
        "python" => skeletonize_python(content),
        "rust" | "javascript" | "typescript" | "go" | "java" | "c" | "cpp"
        | "csharp" | "php" | "swift" | "kotlin" | "scala" => skeletonize_braced(content, language),
        _ => content.to_string(),
    }
}

fn indent_width(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

// ─── Brace languages ──────────────────────────────────────────────────────────

fn skeletonize_braced(content: &str, language: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    // Only function/method bodies are elided. struct/enum/trait/impl headers
    // and their declarations are kept (they are the structure we want).
    let parsed = RegexParser.parse("skeleton", content, language);

    let mut elide = vec![false; lines.len()];
    for sym in parsed.symbols.iter().filter(|s| s.kind == "function") {
        let start_idx = sym.start_line.saturating_sub(1);
        if start_idx >= lines.len() {
            continue;
        }
        if let Some((open_i, close_i)) = body_span(&lines, start_idx) {
            // Keep the `{` line and the matching `}` line; drop the interior.
            for slot in elide.iter_mut().take(close_i).skip(open_i + 1) {
                *slot = true;
            }
        }
    }

    let mut out = String::with_capacity(content.len() / 2);
    let mut prev_elided = false;
    for (i, line) in lines.iter().enumerate() {
        if elide[i] {
            if !prev_elided {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                out.push_str(&indent);
                out.push_str("...\n");
            }
            prev_elided = true;
        } else {
            out.push_str(line);
            out.push('\n');
            prev_elided = false;
        }
    }
    out
}

/// Find the first balanced `{...}` block at or after `start_idx`.
/// Returns (line index of the opening `{`, line index of the matching `}`).
///
/// Naive brace counting — it does not skip braces inside strings or comments,
/// which is acceptable for a best-effort skeleton.
fn body_span(lines: &[&str], start_idx: usize) -> Option<(usize, usize)> {
    let mut depth: i32 = 0;
    let mut open_line: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(start_idx) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    if open_line.is_none() {
                        open_line = Some(i);
                    }
                    depth += 1;
                }
                '}' if open_line.is_some() => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((open_line.unwrap(), i));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

// ─── Python ─────────────────────────────────────────────────────────────────

fn skeletonize_python(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = String::with_capacity(content.len() / 2);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("def ") || trimmed.starts_with("async def ")) {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        let def_indent = indent_width(line);

        // Emit the signature, which may span multiple lines until it ends in ':'.
        let mut sig = i;
        loop {
            out.push_str(lines[sig]);
            out.push('\n');
            if lines[sig].trim_end().ends_with(':') || sig + 1 >= lines.len() || sig - i > 20 {
                break;
            }
            sig += 1;
        }
        let mut k = sig + 1;

        // Keep a leading docstring, if present.
        let mut doc = k;
        while doc < lines.len() && lines[doc].trim().is_empty() {
            doc += 1;
        }
        if doc < lines.len() {
            let t = lines[doc].trim_start();
            for q in ["\"\"\"", "'''"] {
                if t.starts_with(q) {
                    for blank in k..doc {
                        out.push_str(lines[blank]);
                        out.push('\n');
                    }
                    out.push_str(lines[doc]);
                    out.push('\n');
                    let mut end = doc;
                    if !t[q.len()..].contains(q) {
                        end = doc + 1;
                        while end < lines.len() {
                            out.push_str(lines[end]);
                            out.push('\n');
                            if lines[end].contains(q) {
                                break;
                            }
                            end += 1;
                        }
                    }
                    k = end + 1;
                    break;
                }
            }
        }

        // Elide the remaining (deeper-indented) body with a single placeholder.
        let mut placeholder = false;
        while k < lines.len() {
            if lines[k].trim().is_empty() {
                k += 1;
                continue;
            }
            if indent_width(lines[k]) <= def_indent {
                break;
            }
            if !placeholder {
                out.push_str(&" ".repeat(indent_width(lines[k])));
                out.push_str("...\n");
                placeholder = true;
            }
            k += 1;
        }
        i = k;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_elides_fn_body_keeps_struct() {
        let src = "\
pub struct Point {
    x: i32,
    y: i32,
}

/// Adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum
}
";
        let sk = skeletonize(src, "rust");
        // Struct fields kept.
        assert!(sk.contains("x: i32"));
        assert!(sk.contains("y: i32"));
        // Signature + doc kept.
        assert!(sk.contains("pub fn add(a: i32, b: i32) -> i32 {"));
        assert!(sk.contains("/// Adds two numbers."));
        // Body elided.
        assert!(!sk.contains("let sum = a + b;"));
        assert!(sk.contains("..."));
        // Closing brace kept.
        assert!(sk.contains("}"));
    }

    #[test]
    fn rust_keeps_methods_in_impl_elides_their_bodies() {
        let src = "\
impl Point {
    pub fn norm(&self) -> i32 {
        self.x * self.x + self.y * self.y
    }
}
";
        let sk = skeletonize(src, "rust");
        assert!(sk.contains("impl Point {"));
        assert!(sk.contains("pub fn norm(&self) -> i32 {"));
        assert!(!sk.contains("self.x * self.x"));
    }

    #[test]
    fn python_elides_body_keeps_docstring() {
        let src = "\
class Foo:
    def bar(self, n):
        \"\"\"Return n doubled.\"\"\"
        result = n * 2
        return result
";
        let sk = skeletonize(src, "python");
        assert!(sk.contains("class Foo:"));
        assert!(sk.contains("def bar(self, n):"));
        assert!(sk.contains("Return n doubled."));
        assert!(!sk.contains("result = n * 2"));
        assert!(sk.contains("..."));
    }

    #[test]
    fn unknown_language_is_noop() {
        let src = "some plain text\nwith lines\n";
        assert_eq!(skeletonize(src, "text"), src);
    }
}

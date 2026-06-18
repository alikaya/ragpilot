//! Tree-sitter backed parser. Currently Rust-only; every other language falls
//! back to the regex parser. Tree-sitter parses to a concrete syntax tree, so
//! symbol boundaries, nested items (methods inside `impl`, items inside `mod`)
//! and call sites are extracted from real grammar nodes rather than line
//! prefixes — exact `start_line`/`end_line`, and cross-file call edges that the
//! same-file-only regex heuristic misses.
//!
//! It is a *parser*, not a type resolver: callee → definition is still matched
//! by name downstream (the symbol graph), not by semantic resolution.

use std::collections::HashSet;
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser as TsParser, Query, QueryCursor};

use super::{CallRef, Import, ParsedFile, Parser, RegexParser, Symbol, SymbolDetail};

pub struct TreeSitterParser {
    fallback: RegexParser,
}

impl TreeSitterParser {
    pub fn new() -> Self {
        Self { fallback: RegexParser }
    }
}

impl Parser for TreeSitterParser {
    fn parse(&self, path: &str, content: &str, language: &str) -> ParsedFile {
        match language {
            "rust" => parse_rust(path, content)
                .unwrap_or_else(|| self.fallback.parse(path, content, language)),
            _ => self.fallback.parse(path, content, language),
        }
    }
}

// ─── Rust ──────────────────────────────────────────────────────────────────────

struct RustLang {
    language: tree_sitter::Language,
    symbols:  Query,
    calls:    Query,
    uses:     Query,
}

const SYMBOL_QUERY: &str = r#"
(function_item   name: (identifier)      @name) @function
(struct_item     name: (type_identifier) @name) @struct
(union_item      name: (type_identifier) @name) @struct
(enum_item       name: (type_identifier) @name) @enum
(trait_item      name: (type_identifier) @name) @trait
(mod_item        name: (identifier)      @name) @module
(const_item      name: (identifier)      @name) @const
(static_item     name: (identifier)      @name) @static
(type_item       name: (type_identifier) @name) @type
(macro_definition name: (identifier)     @name) @macro
(impl_item       type: (type_identifier) @name) @impl
(impl_item       type: (generic_type type: (type_identifier) @name)) @impl
"#;

const CALL_QUERY: &str = r#"
(call_expression function: (identifier) @callee)
(call_expression function: (scoped_identifier name: (identifier) @callee))
(call_expression function: (field_expression field: (field_identifier) @callee))
(call_expression function: (generic_function function: (identifier) @callee))
(macro_invocation macro: (identifier) @callee)
"#;

const USE_QUERY: &str = r#"(use_declaration) @use"#;

fn rust_lang() -> &'static RustLang {
    static L: OnceLock<RustLang> = OnceLock::new();
    L.get_or_init(|| {
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let symbols = Query::new(&language, SYMBOL_QUERY).expect("rust symbol query");
        let calls   = Query::new(&language, CALL_QUERY).expect("rust call query");
        let uses    = Query::new(&language, USE_QUERY).expect("rust use query");
        RustLang { language, symbols, calls, uses }
    })
}

/// Returns `None` if tree-sitter fails to parse, so the caller falls back.
fn parse_rust(path: &str, content: &str) -> Option<ParsedFile> {
    let rl = rust_lang();
    let mut parser = TsParser::new();
    parser.set_language(&rl.language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();

    let symbols = collect_symbols(path, content, src, root, rl);
    let imports = collect_imports(path, content, src, root, rl);
    let calls   = collect_calls(content, src, root, rl, &symbols);

    Some(ParsedFile { path: path.to_string(), symbols, imports, calls })
}

/// Extract per-symbol details (signature + body) for Rust. Returns `None` if
/// tree-sitter fails to parse. Used by the semantic-diff tool.
pub fn rust_details(content: &str) -> Option<Vec<SymbolDetail>> {
    let rl = rust_lang();
    let mut parser = TsParser::new();
    parser.set_language(&rl.language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();
    let names = rl.symbols.capture_names();

    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut it = cursor.matches(&rl.symbols, root, src);
    while let Some(m) = it.next() {
        let mut name: Option<String> = None;
        let mut kind: Option<&str> = None;
        let mut node: Option<Node> = None;
        for cap in m.captures {
            let cn = names[cap.index as usize];
            if cn == "name" {
                name = Some(content[cap.node.byte_range()].to_string());
            } else {
                kind = Some(cn);
                node = Some(cap.node);
            }
        }
        if let (Some(name), Some(kind), Some(node)) = (name, kind, node) {
            let full = content[node.byte_range()].to_string();
            let signature = if kind == "function" {
                match node.child_by_field_name("body") {
                    Some(body) => normalize_ws(&content[node.start_byte()..body.start_byte()]),
                    None => normalize_ws(first_line(&full)),
                }
            } else {
                normalize_ws(first_line(&full))
            };
            out.push(SymbolDetail {
                name,
                kind: kind.to_string(),
                signature,
                body: full,
                start_line: node.start_position().row + 1,
            });
        }
    }
    Some(out)
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or(s)
}

/// Collapse all runs of whitespace (incl. newlines) into single spaces so that
/// multi-line signatures compare cleanly.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_symbols(path: &str, content: &str, src: &[u8], root: Node, rl: &RustLang) -> Vec<Symbol> {
    let names = rl.symbols.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(&rl.symbols, root, src);
    while let Some(m) = it.next() {
        let mut name: Option<String> = None;
        let mut kind: Option<&str> = None;
        let mut start = 0usize;
        let mut end = 0usize;

        for cap in m.captures {
            let cap_name = names[cap.index as usize];
            if cap_name == "name" {
                name = Some(content[cap.node.byte_range()].to_string());
            } else {
                kind = Some(cap_name);
                start = cap.node.start_position().row + 1;
                end = cap.node.end_position().row + 1;
            }
        }

        if let (Some(name), Some(kind)) = (name, kind) {
            out.push(Symbol {
                id: format!("{path}::{name}"),
                path: path.to_string(),
                name,
                kind: kind.to_string(),
                start_line: start,
                end_line: end.max(start),
            });
        }
    }

    out
}

fn collect_imports(path: &str, content: &str, src: &[u8], root: Node, rl: &RustLang) -> Vec<Import> {
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(&rl.uses, root, src);
    while let Some(m) = it.next() {
        for cap in m.captures {
            let text = &content[cap.node.byte_range()];
            for (from_module, symbol_name) in expand_rust_use(text) {
                out.push(Import {
                    importer: path.to_string(),
                    from_module,
                    symbol_name,
                });
            }
        }
    }

    out
}

fn collect_calls(
    content: &str,
    src: &[u8],
    root: Node,
    rl: &RustLang,
    symbols: &[Symbol],
) -> Vec<CallRef> {
    let names = rl.calls.capture_names();
    let mut cursor = QueryCursor::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(&rl.calls, root, src);
    while let Some(m) = it.next() {
        for cap in m.captures {
            if names[cap.index as usize] != "callee" {
                continue;
            }
            let callee = content[cap.node.byte_range()].to_string();
            let line = cap.node.start_position().row + 1;
            let Some(caller_id) = enclosing_symbol(symbols, line) else { continue };

            // Dedup per (caller, callee) so a function that calls `clone` 50×
            // produces one edge, not fifty.
            if seen.insert((caller_id.clone(), callee.clone())) {
                out.push(CallRef { caller_id, callee_name: callee, call_line: line });
            }
        }
    }

    out
}

/// The innermost defined symbol whose line span contains `line` (e.g. a method
/// rather than its enclosing `impl`). Returns the symbol id.
fn enclosing_symbol(symbols: &[Symbol], line: usize) -> Option<String> {
    symbols
        .iter()
        .filter(|s| s.start_line <= line && line <= s.end_line)
        // innermost = the one that starts latest
        .max_by_key(|s| s.start_line)
        .map(|s| s.id.clone())
}

/// Expand the text of a `use` declaration into (from_module, symbol_name) pairs.
/// Handles `use a::b::c;`, `use a::b::{c, d};`, `use a::b::*;`, `use a::b as d;`.
fn expand_rust_use(decl: &str) -> Vec<(String, String)> {
    let body = decl
        .trim()
        .trim_start_matches("use")
        .trim()
        .trim_end_matches(';')
        .trim();

    // Brace group: prefix::{a, b::c, self}
    if let Some(open) = body.find('{') {
        let prefix = body[..open].trim_end_matches(':').trim_end_matches(':').trim();
        let inner = &body[open + 1..body.rfind('}').unwrap_or(body.len())];
        return inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|item| {
                let module = if prefix.is_empty() {
                    item.to_string()
                } else if item == "self" {
                    prefix.to_string()
                } else {
                    format!("{prefix}::{item}")
                };
                let last = leaf(&module);
                (module, last)
            })
            .collect();
    }

    // `as` alias → the bound name is the alias
    if let Some(idx) = body.find(" as ") {
        let module = body[..idx].trim().to_string();
        let alias = body[idx + 4..].trim().to_string();
        return vec![(module, alias)];
    }

    let module = body.to_string();
    let name = if module.ends_with('*') { "*".to_string() } else { leaf(&module) };
    if module.is_empty() {
        vec![]
    } else {
        vec![(module, name)]
    }
}

fn leaf(module: &str) -> String {
    module
        .trim_end_matches("::*")
        .rsplit("::")
        .next()
        .unwrap_or("*")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ParsedFile {
        TreeSitterParser::new().parse("src/x.rs", src, "rust")
    }

    #[test]
    fn extracts_symbols_with_exact_spans() {
        let p = parse("pub fn alpha() {\n    let x = 1;\n}\n\nstruct Bar {\n    n: u32,\n}\n");
        let alpha = p.symbols.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.kind, "function");
        assert_eq!(alpha.start_line, 1);
        assert_eq!(alpha.end_line, 3); // real closing brace, not a +60 guess
        let bar = p.symbols.iter().find(|s| s.name == "Bar").unwrap();
        assert_eq!(bar.kind, "struct");
        assert_eq!(bar.start_line, 5);
        assert_eq!(bar.end_line, 7);
    }

    #[test]
    fn extracts_method_in_impl_as_caller() {
        let src = "struct S;\nimpl S {\n    fn run(&self) {\n        helper();\n    }\n}\nfn helper() {}\n";
        let p = parse(src);
        // the call to helper() is attributed to run (the method), not the impl
        let call = p.calls.iter().find(|c| c.callee_name == "helper").unwrap();
        assert_eq!(call.caller_id, "src/x.rs::run");
        assert_eq!(call.call_line, 4);
    }

    #[test]
    fn expands_use_braces_and_alias() {
        let p = parse("use a::b::{c, d};\nuse x::y as z;\nuse foo::bar::*;\n");
        let pairs: Vec<(String, String)> = p.imports.iter()
            .map(|i| (i.from_module.clone(), i.symbol_name.clone()))
            .collect();
        assert!(pairs.contains(&("a::b::c".into(), "c".into())));
        assert!(pairs.contains(&("a::b::d".into(), "d".into())));
        assert!(pairs.contains(&("x::y".into(), "z".into())));
        assert!(pairs.iter().any(|(m, n)| m == "foo::bar::*" && n == "*"));
    }

    #[test]
    fn falls_back_for_other_languages() {
        let p = TreeSitterParser::new().parse("a.py", "def foo():\n    pass\n", "python");
        assert!(p.symbols.iter().any(|s| s.name == "foo"));
    }
}

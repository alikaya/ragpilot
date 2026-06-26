//! Tree-sitter backed parser. Table-driven and multi-language: Rust, Python,
//! JavaScript, TypeScript/TSX, Go, Java, C, C++, C#, Ruby and PHP are parsed
//! from a concrete syntax tree; any other language (or a file tree-sitter
//! cannot parse) falls back to the regex parser.
//!
//! Two extraction strategies feed one shared engine:
//!
//! * **Rust** uses a bespoke pair of symbol/call queries — it is the dogfood
//!   language and needs full fidelity (struct / enum / trait / const / static
//!   distinctions and the `rust_details` signature extraction the semantic-diff
//!   tool relies on).
//! * **Every other language** is driven by the grammar's own bundled
//!   `TAGS_QUERY` (the standard `tags.scm` convention: `@definition.*` /
//!   `@reference.call` / `@name`). This is the scalability lever — adding a new
//!   language is just "register its grammar + `TAGS_QUERY`", with no
//!   hand-written queries to maintain. The taxonomy is whatever the grammar's
//!   tags.scm defines (e.g. a C `struct` is tagged `class`).
//!
//! Imports are a separate concern (tags.scm does not capture them), so each
//! language may also supply a small hand-written import query.
//!
//! The extraction logic is language-agnostic and keys only off capture names
//! (`@name`, `@definition.*`, `@reference.call`, `@callee`, `@module`). A
//! language whose query fails to compile is skipped (its files fall back to
//! regex) rather than panicking, so a bad grammar/query degrades gracefully.
//!
//! It is a *parser*, not a type resolver: callee → definition is still matched
//! by name downstream (the symbol graph), not by semantic resolution.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser as TsParser, Query, QueryCursor};

use super::{CallRef, Import, ParsedFile, Parser, RegexParser, Symbol, SymbolDetail};

pub struct TreeSitterParser {
    fallback:  RegexParser,
    /// Per-project query overrides, keyed by language. Empty for `new()`.
    overrides: HashMap<String, LangSpec>,
}

impl TreeSitterParser {
    /// Parser using the built-in (embedded + grammar-upstream) queries.
    pub fn new() -> Self {
        Self { fallback: RegexParser, overrides: HashMap::new() }
    }

    /// Parser that prefers per-project query overrides found under `dir`
    /// (`<dir>/<lang>/{tags,symbols,calls,imports}.scm`), falling back to the
    /// built-in query for any slot or language without an override file. A
    /// missing directory simply yields the built-in behaviour.
    pub fn with_query_overrides(dir: &Path) -> Self {
        let overrides = if dir.is_dir() { build_overrides(dir) } else { HashMap::new() };
        Self { fallback: RegexParser, overrides }
    }
}

impl Parser for TreeSitterParser {
    fn parse(&self, path: &str, content: &str, language: &str) -> ParsedFile {
        let spec = self.overrides.get(language).or_else(|| lang_spec(language));
        if let Some(spec) = spec {
            if let Some(parsed) = parse_with(spec, path, content) {
                return parsed;
            }
        }
        self.fallback.parse(path, content, language)
    }
}

// ─── Engine ──────────────────────────────────────────────────────────────────

/// How a language's import query maps captured nodes to `(module, symbol)`.
#[derive(Clone, Copy)]
enum ImportStyle {
    /// Rust `use` declarations — the whole node is captured and expanded
    /// (`use a::b::{c, d}` → two imports), handled by [`expand_rust_use`].
    RustUse,
    /// A single `@module` capture holding a module path or string literal
    /// (`import react`, `#include <stdio.h>`, `import java.util.List`).
    ModulePath,
}

struct Uses {
    query: Query,
    style: ImportStyle,
}

/// How a language's symbols and calls are extracted.
enum Extractor {
    /// Bespoke definition + call queries (Rust): full taxonomy + body spans.
    Bespoke { symbols: Query, calls: Query },
    /// The grammar's bundled `tags.scm` (`@definition.*` / `@reference.call`).
    Tags(Query),
}

/// A compiled grammar plus its extraction strategy and optional import query.
struct LangSpec {
    language:  Language,
    extractor: Extractor,
    uses:      Option<Uses>,
}

/// Static description of a language: grammar, how to extract symbols/calls, and
/// an optional import query. The query strings are *defaults* (ragpilot-embedded
/// or the grammar's upstream `TAGS_QUERY`); each slot can be overridden by a
/// file at parse time.
struct LangDef {
    name:      &'static str,
    language:  Language,
    extractor: ExtractorDef,
    import:    Option<(&'static str, ImportStyle)>,
}

enum ExtractorDef {
    Bespoke { symbols: &'static str, calls: &'static str },
    Tags(&'static str),
}

/// The full language table. Adding a language is a single entry here (plus the
/// grammar crate in `Cargo.toml`) — for most languages just the grammar's
/// `TAGS_QUERY` and a small import query.
fn lang_defs() -> Vec<LangDef> {
    use ExtractorDef::{Bespoke, Tags};
    use ImportStyle::{ModulePath, RustUse};

    vec![
        LangDef { name: "rust", language: tree_sitter_rust::LANGUAGE.into(),
            extractor: Bespoke { symbols: RUST_SYM, calls: RUST_CALL },
            import: Some((RUST_USE, RustUse)) },
        LangDef { name: "python", language: tree_sitter_python::LANGUAGE.into(),
            extractor: Tags(tree_sitter_python::TAGS_QUERY),
            import: Some((PY_USE, ModulePath)) },
        LangDef { name: "javascript", language: tree_sitter_javascript::LANGUAGE.into(),
            extractor: Tags(tree_sitter_javascript::TAGS_QUERY),
            import: Some((JS_USE, ModulePath)) },
        LangDef { name: "typescript", language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            extractor: Tags(TS_TAGS),
            import: Some((TS_USE, ModulePath)) },
        LangDef { name: "go", language: tree_sitter_go::LANGUAGE.into(),
            extractor: Tags(tree_sitter_go::TAGS_QUERY),
            import: Some((GO_USE, ModulePath)) },
        LangDef { name: "java", language: tree_sitter_java::LANGUAGE.into(),
            extractor: Tags(tree_sitter_java::TAGS_QUERY),
            import: Some((JAVA_USE, ModulePath)) },
        LangDef { name: "c", language: tree_sitter_c::LANGUAGE.into(),
            extractor: Tags(tree_sitter_c::TAGS_QUERY),
            import: Some((C_USE, ModulePath)) },
        LangDef { name: "cpp", language: tree_sitter_cpp::LANGUAGE.into(),
            extractor: Tags(tree_sitter_cpp::TAGS_QUERY),
            import: Some((CPP_USE, ModulePath)) },
        LangDef { name: "csharp", language: tree_sitter_c_sharp::LANGUAGE.into(),
            extractor: Tags(tree_sitter_c_sharp::TAGS_QUERY),
            import: Some((CS_USE, ModulePath)) },
        LangDef { name: "ruby", language: tree_sitter_ruby::LANGUAGE.into(),
            extractor: Tags(tree_sitter_ruby::TAGS_QUERY),
            import: None },
        LangDef { name: "php", language: tree_sitter_php::LANGUAGE_PHP.into(),
            extractor: Tags(tree_sitter_php::TAGS_QUERY),
            import: Some((PHP_USE, ModulePath)) },
    ]
}

/// Compile a `LangSpec` from a `LangDef`, optionally overriding any query slot
/// from `<dir>/<lang>/<slot>.scm` (slots: `symbols`, `calls`, `tags`,
/// `imports`). The extraction query is mandatory (→ `None` on failure, so the
/// language falls back to regex); a bad import query degrades to no imports.
fn build_lang_spec(def: &LangDef, dir: Option<&Path>) -> Option<LangSpec> {
    let resolve = |slot: &str, default: &str| -> String {
        if let Some(d) = dir {
            let path = d.join(def.name).join(format!("{slot}.scm"));
            if let Ok(text) = std::fs::read_to_string(&path) {
                tracing::info!(
                    "tree-sitter: '{}' {slot} query overridden from {}",
                    def.name, path.display());
                return text;
            }
        }
        default.to_string()
    };

    let extractor = match &def.extractor {
        ExtractorDef::Bespoke { symbols, calls } => Extractor::Bespoke {
            symbols: Query::new(&def.language, &resolve("symbols", symbols)).ok()?,
            calls:   Query::new(&def.language, &resolve("calls", calls)).ok()?,
        },
        ExtractorDef::Tags(tags) => {
            Extractor::Tags(Query::new(&def.language, &resolve("tags", tags)).ok()?)
        }
    };
    let uses = def.import.and_then(|(q, style)| {
        Query::new(&def.language, &resolve("imports", q))
            .ok()
            .map(|query| Uses { query, style })
    });

    Some(LangSpec { language: def.language.clone(), extractor, uses })
}

/// Process-wide default registry (embedded/upstream queries, no overrides).
/// Used for files without a per-project override and by `rust_details`.
fn lang_spec(language: &str) -> Option<&'static LangSpec> {
    static REGISTRY: OnceLock<HashMap<String, LangSpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| build_registry(None)).get(language)
}

fn build_registry(dir: Option<&Path>) -> HashMap<String, LangSpec> {
    let mut m = HashMap::new();
    for def in lang_defs() {
        match build_lang_spec(&def, dir) {
            Some(spec) => { m.insert(def.name.to_string(), spec); }
            None => tracing::warn!(
                "tree-sitter: language '{}' disabled (query failed to compile)", def.name),
        }
    }
    m
}

/// Per-instance override registry: only languages with a `<dir>/<lang>/`
/// directory get a (possibly partially overridden) spec; everything else keeps
/// using the process-wide default.
fn build_overrides(dir: &Path) -> HashMap<String, LangSpec> {
    let mut m = HashMap::new();
    for def in lang_defs() {
        if !dir.join(def.name).is_dir() {
            continue;
        }
        match build_lang_spec(&def, Some(dir)) {
            Some(spec) => { m.insert(def.name.to_string(), spec); }
            None => tracing::warn!(
                "tree-sitter: override for '{}' failed to compile; using default", def.name),
        }
    }
    m
}

fn parse_with(spec: &LangSpec, path: &str, content: &str) -> Option<ParsedFile> {
    let mut parser = TsParser::new();
    parser.set_language(&spec.language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();

    let (symbols, calls) = match &spec.extractor {
        Extractor::Bespoke { symbols, calls } => {
            let syms = collect_symbols(path, content, src, root, symbols);
            let cs = collect_calls(content, src, root, calls, &syms);
            (syms, cs)
        }
        Extractor::Tags(q) => collect_from_tags(path, content, src, root, q),
    };
    let imports = collect_imports(path, content, src, root, spec.uses.as_ref());

    Some(ParsedFile { path: path.to_string(), symbols, imports, calls })
}

/// Generic extraction from a `tags.scm` query: `@definition.<kind>` (with a
/// child `@name`) yields symbols with full node spans; `@reference.call` /
/// `@reference.send` (with `@name` = callee) yield call edges attributed to the
/// enclosing symbol. Bare `@name` / `@reference.<other>` / `@doc` captures are
/// ignored, so navigation-only tags don't pollute the symbol graph.
fn collect_from_tags(
    path: &str,
    content: &str,
    src: &[u8],
    root: Node,
    query: &Query,
) -> (Vec<Symbol>, Vec<CallRef>) {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut symbols = Vec::new();
    let mut call_sites: Vec<(String, usize)> = Vec::new();

    let mut it = cursor.matches(query, root, src);
    while let Some(m) = it.next() {
        let mut name: Option<(String, Node)> = None;
        let mut def: Option<(String, Node)> = None;
        let mut is_call = false;

        for cap in m.captures {
            let cn = names[cap.index as usize];
            if cn == "name" {
                name = Some((content[cap.node.byte_range()].to_string(), cap.node));
            } else if let Some(kind) = cn.strip_prefix("definition.") {
                def = Some((kind.to_string(), cap.node));
            } else if cn == "reference.call" || cn == "reference.send" {
                is_call = true;
            }
        }

        match (name, def) {
            (Some((nm, _)), Some((kind, node))) => {
                let start = node.start_position().row + 1;
                let end = node.end_position().row + 1;
                symbols.push(Symbol {
                    id: format!("{path}::{nm}"),
                    path: path.to_string(),
                    name: nm,
                    kind,
                    start_line: start,
                    end_line: end.max(start),
                });
            }
            (Some((nm, nnode)), None) if is_call => {
                call_sites.push((nm, nnode.start_position().row + 1));
            }
            _ => {}
        }
    }

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut calls = Vec::new();
    for (callee, line) in call_sites {
        let Some(caller_id) = enclosing_symbol(&symbols, line) else { continue };
        if seen.insert((caller_id.clone(), callee.clone())) {
            calls.push(CallRef { caller_id, callee_name: callee, call_line: line });
        }
    }

    (symbols, calls)
}

// ─── Extraction (language-agnostic) ──────────────────────────────────────────

fn collect_symbols(path: &str, content: &str, src: &[u8], root: Node, query: &Query) -> Vec<Symbol> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(query, root, src);
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

fn collect_imports(
    path: &str,
    content: &str,
    src: &[u8],
    root: Node,
    uses: Option<&Uses>,
) -> Vec<Import> {
    let Some(uses) = uses else { return Vec::new() };
    let names = uses.query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(&uses.query, root, src);
    while let Some(m) = it.next() {
        for cap in m.captures {
            match uses.style {
                ImportStyle::RustUse => {
                    let text = &content[cap.node.byte_range()];
                    for (from_module, symbol_name) in expand_rust_use(text) {
                        out.push(Import {
                            importer: path.to_string(),
                            from_module,
                            symbol_name,
                        });
                    }
                }
                ImportStyle::ModulePath => {
                    if names[cap.index as usize] != "module" {
                        continue;
                    }
                    let module = strip_quotes(&content[cap.node.byte_range()]).to_string();
                    if module.is_empty() {
                        continue;
                    }
                    let symbol_name = path_leaf(&module);
                    out.push(Import {
                        importer: path.to_string(),
                        from_module: module,
                        symbol_name,
                    });
                }
            }
        }
    }

    out
}

fn collect_calls(
    content: &str,
    src: &[u8],
    root: Node,
    query: &Query,
    symbols: &[Symbol],
) -> Vec<CallRef> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();

    let mut it = cursor.matches(query, root, src);
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

/// Strip surrounding string/include delimiters from a captured module token.
fn strip_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '<' || c == '>')
}

/// Last path segment of a module reference, splitting on `/`, `.` or `\`.
/// `java.util.List` → `List`, `./utils/fs` → `fs`, `react` → `react`.
fn path_leaf(module: &str) -> String {
    let trimmed = module.trim_end_matches(';').trim();
    trimmed
        .rsplit(|c| c == '/' || c == '.' || c == '\\')
        .find(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

// ─── Semantic diff details (Rust) ────────────────────────────────────────────

/// Extract per-symbol details (signature + body) for Rust. Returns `None` if
/// tree-sitter fails to parse. Used by the semantic-diff tool.
pub fn rust_details(content: &str) -> Option<Vec<SymbolDetail>> {
    let spec = lang_spec("rust")?;
    let Extractor::Bespoke { symbols, .. } = &spec.extractor else { return None };
    let mut parser = TsParser::new();
    parser.set_language(&spec.language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();
    let names = symbols.capture_names();

    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut it = cursor.matches(symbols, root, src);
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

// ─── Per-language queries ────────────────────────────────────────────────────

// Queries live as `.scm` files under `queries/<lang>/` and are embedded at
// build time, so the binary stays self-contained. Languages not listed here
// (python, javascript, go, java, c, cpp, csharp, ruby, php) extract symbols and
// calls from the grammar's own bundled `TAGS_QUERY`; only their import query is
// shipped by ragpilot. Any slot can be overridden at runtime — see
// [`TreeSitterParser::with_query_overrides`].

const RUST_SYM: &str = include_str!("../../queries/rust/symbols.scm");
const RUST_CALL: &str = include_str!("../../queries/rust/calls.scm");
const RUST_USE: &str = include_str!("../../queries/rust/imports.scm");

// TypeScript: the grammar's bundled tags.scm is signature/`.d.ts`-oriented and
// misses ordinary `class`/`function`/`method` declarations, so ragpilot ships
// its own tags query (the "override when upstream tags is insufficient" case).
const TS_TAGS: &str = include_str!("../../queries/typescript/tags.scm");

const PY_USE: &str = include_str!("../../queries/python/imports.scm");
const JS_USE: &str = include_str!("../../queries/javascript/imports.scm");
const TS_USE: &str = include_str!("../../queries/typescript/imports.scm");
const GO_USE: &str = include_str!("../../queries/go/imports.scm");
const JAVA_USE: &str = include_str!("../../queries/java/imports.scm");
const C_USE: &str = include_str!("../../queries/c/imports.scm");
const CPP_USE: &str = include_str!("../../queries/cpp/imports.scm");
const CS_USE: &str = include_str!("../../queries/csharp/imports.scm");
const PHP_USE: &str = include_str!("../../queries/php/imports.scm");

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ParsedFile {
        TreeSitterParser::new().parse("src/x.rs", src, "rust")
    }

    fn parse_lang(src: &str, language: &str) -> ParsedFile {
        TreeSitterParser::new().parse("src/x", src, language)
    }

    fn has(p: &ParsedFile, name: &str, kind: &str) -> bool {
        p.symbols.iter().any(|s| s.name == name && s.kind == kind)
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
    fn parses_python() {
        let p = parse_lang("import os\nfrom a.b import c\n\nclass Foo:\n    def bar(self):\n        helper()\n", "python");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "bar", "function"));
        assert!(p.calls.iter().any(|c| c.callee_name == "helper" && c.caller_id == "src/x::bar"));
        assert!(p.imports.iter().any(|i| i.from_module == "os"));
        assert!(p.imports.iter().any(|i| i.from_module == "a.b"));
    }

    #[test]
    fn parses_javascript() {
        let p = parse_lang("import x from \"react\";\nclass Foo {}\nfunction bar() { baz(); }\n", "javascript");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "bar", "function"));
        assert!(p.calls.iter().any(|c| c.callee_name == "baz"));
        assert!(p.imports.iter().any(|i| i.from_module == "react"));
    }

    #[test]
    fn parses_typescript() {
        let p = parse_lang("interface I { x: number }\nclass C {}\nfunction f(): void {}\n", "typescript");
        assert!(has(&p, "I", "interface"));
        assert!(has(&p, "C", "class"));
        assert!(has(&p, "f", "function"));
    }

    #[test]
    fn parses_go() {
        let p = parse_lang("package main\nimport \"fmt\"\nfunc Foo() { fmt.Println() }\ntype Bar struct{}\n", "go");
        assert!(has(&p, "Foo", "function"));
        assert!(has(&p, "Bar", "type"));
        assert!(p.imports.iter().any(|i| i.from_module == "fmt"));
    }

    #[test]
    fn parses_java() {
        let p = parse_lang("import java.util.List;\nclass Foo { void bar() { baz(); } }\n", "java");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "bar", "method"));
        assert!(p.calls.iter().any(|c| c.callee_name == "baz"));
        assert!(p.imports.iter().any(|i| i.from_module == "java.util.List"));
    }

    #[test]
    fn parses_c() {
        let p = parse_lang("#include <stdio.h>\nstruct Pt { int x; };\nint main() { return 0; }\n", "c");
        assert!(has(&p, "main", "function"));
        // C's tags.scm tags structs as `class` (standard tags taxonomy).
        assert!(has(&p, "Pt", "class"));
    }

    #[test]
    fn parses_cpp() {
        let p = parse_lang("class Foo {};\nint bar() { return 0; }\n", "cpp");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "bar", "function"));
    }

    #[test]
    fn parses_csharp() {
        let p = parse_lang("class Foo { void Bar() {} }\n", "csharp");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "Bar", "method"));
    }

    #[test]
    fn parses_ruby() {
        let p = parse_lang("class Foo\n  def bar\n    baz\n  end\nend\n", "ruby");
        assert!(has(&p, "Foo", "class"));
        assert!(has(&p, "bar", "method"));
    }

    #[test]
    fn parses_php() {
        let p = parse_lang("<?php\nclass Foo {\n  function bar() {}\n}\n", "php");
        assert!(has(&p, "Foo", "class"));
        // PHP's tags.scm tags methods as `function` (standard tags taxonomy).
        assert!(has(&p, "bar", "function"));
    }

    #[test]
    fn query_override_from_dir_takes_effect() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("ragpilot_qover_{}", std::process::id()));
        let pydir = base.join("python");
        fs::create_dir_all(&pydir).unwrap();
        // Override: tag function definitions with a custom kind the built-in
        // query never produces, so a match proves the override was used.
        fs::write(
            pydir.join("tags.scm"),
            "(function_definition name: (identifier) @name) @definition.routine\n",
        )
        .unwrap();

        let parser = TreeSitterParser::with_query_overrides(&base);
        let p = parser.parse("a.py", "def foo():\n    pass\n", "python");
        assert!(
            p.symbols.iter().any(|s| s.name == "foo" && s.kind == "routine"),
            "override query should tag foo as 'routine', got {:?}", p.symbols
        );

        // A language with no override directory still uses the built-in query.
        let q = parser.parse("a.rb", "class Foo\nend\n", "ruby");
        assert!(q.symbols.iter().any(|s| s.name == "Foo" && s.kind == "class"));

        fs::remove_dir_all(&base).ok();
    }
}

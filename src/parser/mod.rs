use serde::{Deserialize, Serialize};

pub mod regex_parser;
pub mod tree_sitter_parser;
pub use regex_parser::RegexParser;
pub use tree_sitter_parser::TreeSitterParser;

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// Unique ID: "relative/path::symbol_name"
    pub id:         String,
    pub path:       String,
    pub name:       String,
    pub kind:       String,   // "function" | "class" | "struct" | "trait" | "enum" | "impl" | ...
    pub start_line: usize,    // 1-based
    pub end_line:   usize,    // estimated 1-based
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    /// Source file doing the importing
    pub importer: String,
    /// Module / file being imported from (may be a path or module name)
    pub from_module: String,
    /// Specific symbol being imported, or "*" for wildcard
    pub symbol_name: String,
}

/// Call reference: caller_id calls callee_name at call_line
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRef {
    pub caller_id:   String,
    pub callee_name: String,
    pub call_line:   usize,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFile {
    pub path:    String,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub calls:   Vec<CallRef>,
}

/// A symbol plus its signature and source body, used by the semantic-diff tool
/// to detect signature/return-type changes between two versions of a file.
#[derive(Debug, Clone)]
pub struct SymbolDetail {
    pub name:       String,
    pub kind:       String,
    /// Declaration without the body — for Rust functions this is
    /// `fn name(params) -> Ret`; for other kinds, the declaration line.
    pub signature:  String,
    /// Full source text of the symbol (for body-change detection).
    pub body:       String,
    pub start_line: usize,
}

// ─── Trait ───────────────────────────────────────────────────────────────────

pub trait Parser: Send + Sync {
    fn parse(&self, path: &str, content: &str, language: &str) -> ParsedFile;
}

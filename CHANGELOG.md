# Changelog

All notable changes to **ragpilot** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/) and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-06-18

### Added
- **Project-aware `init`** — detects the project's languages and source
  directories and pre-fills `include_extensions` / `include_dirs`
  interactively, confirming sensible defaults. Falls back to pure
  auto-detection when stdin/stdout is not a TTY (scripts, agent-driven
  `setup`), so it never blocks. Fixes empty indexes (`Indexed 0 of 0`) on
  non-Rust or non-`src/` projects, where the old hardcoded filters matched
  nothing.
- **Symbols & Graph dashboard** in `ragpilot status` — symbol / call / import
  totals, a by-kind breakdown, hot (most-called) project symbols, and the
  largest files by symbol count, rendered with bar charts from the symbol
  graph.
- **Tree-sitter Rust parser** behind the existing parser trait — exact symbol
  spans, methods inside `impl`, and cross-file call edges that the regex
  heuristic missed (≈12× more call edges measured on this repo). Every other
  language falls back to the regex parser.
- **Semantic diff** — `ragpilot review [<ref>]` (CLI) and the
  `review.semantic_diff` MCP tool. Classifies changes per symbol
  (added / removed / signature_changed / modified) and attaches the blast
  radius (callers from the symbol graph, dependent files from the import
  graph) for PR review and commit-message generation. Defaults to the working
  tree vs `HEAD`; accepts a ref (`HEAD~1`) or range (`main..HEAD`).
- **AST-style context pruning** — `ragpilot skeleton <file>` (CLI) and the
  `rag.get_skeleton` MCP tool render a token-efficient file skeleton
  (signatures, type/struct definitions, imports, doc comments) with bodies
  elided to `...`.

### Fixed
- Symbol-graph orphan cleanup: deleted/moved files no longer leave stale
  symbols behind. `remove_file` now also prunes the symbol graph, and every
  index run self-heals orphans whose path is no longer scanned — keeping
  `nav.symbol_resolve` / `nav.call_graph` / `impact.analyze` and the dashboard
  accurate.
- `ragpilot init --force` now drives a full re-index **without** clobbering a
  user-customized `.rag/config.toml`.

### Changed
- `Cargo.lock` is now tracked (dependencies added → reproducible builds).

## [0.1.0] - 2026-06-18

### Added
- Initial release: local RAG MCP server for Claude Code / Codex — semantic
  code search, `context.bundle`, symbol graph & call graph navigation, impact
  analysis, honest token-saving metrics, and git post-commit/post-merge
  auto-indexing.

# Changelog

All notable changes to **ragpilot** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/) and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-06-20

### Added
- **Multi-client MCP registration** (`src/agents.rs`) — one command,
  `ragpilot init <dir> <agent>`, writes the correct config in each client's
  own format: `claude` (`.mcp.json`), `codex` (`.codex/config.toml`),
  `cursor` (`.cursor/mcp.json`), `vscode` (`.vscode/mcp.json`, root key
  `servers`), and `opencode` (`opencode.json`, root key `mcp` with a command
  array). `all` registers every project client at once.
- **Antigravity CLI support** — Google retired the Gemini CLI on 2026-06-18 in
  favour of the Antigravity CLI (binary `agy`). `antigravity` is now a
  first-class target; `gemini` is accepted as a deprecated alias that
  redirects to it. Antigravity and Windsurf are global-only, so `init` prints
  a ready-to-paste snippet and the exact `$HOME` path instead of writing
  outside the repo.

### Changed
- **Underscore tool names** — all MCP tools were renamed from dotted to
  underscored form (`rag.search` → `rag_search`, `context.bundle` →
  `context_bundle`, etc.). Several clients (Antigravity/Gemini, Copilot,
  Cursor) reject or silently drop names containing dots, which left their tool
  lists empty. The dispatcher normalizes any legacy dotted name to its
  underscore form, so older configs keep working.
- **`initialize` echoes the client's `protocolVersion`** so strict newer
  clients negotiate cleanly, falling back to a known-good version otherwise.
- **English init/setup prompts** — the interactive language/directory
  questions shown during `init` are now in English.
- README and `docs/USAGE.md` updated for the new clients, the underscore tool
  names, and the Gemini → Antigravity migration.

### Fixed
- The registered MCP command and server key are now always `ragpilot`, never
  `rag`. Existing configs written by older versions (`.mcp.json`,
  `.codex/config.toml`) are migrated from the legacy `rag` key/command to
  `ragpilot` in place.

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

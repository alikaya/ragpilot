# Changelog

All notable changes to **ragpilot** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/) and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **`ragpilot migrate --scan <dir>`** — a read-only inventory of every legacy
  `.rag/` project under a directory: where it is, what collection it uses, the
  id it will get, how much project-local data it holds, and whether it has been
  migrated already. Upgrading used to mean remembering where every project was
  and visiting each one.
- **`ragpilot migrate --all <dir>`** — migrates them in one pass.

### Fixed
- **Two projects can no longer end up sharing one index.** The old scheme named
  a collection after the *folder*, so `~/dev/api` and `~/tmp/api` were one
  collection. Migrating the second aliased it onto the first's index, and from
  then on both projects searched the same one. Migration now refuses to alias a
  collection another project has already claimed, and bulk migration skips such
  projects and names them.
- **`brain compile` no longer loses the material behind a skipped chunk.** A
  chunk whose model output failed to parse was reported as skipped, but its
  source files were still recorded as compiled — so that day's log was never
  retried and its content was silently gone. Chunks now carry the sources that
  went into them, and only the sources of chunks that actually succeeded are
  recorded.
- **`ragpilot brain session-end` no longer hangs in a terminal.** It reads the
  hook payload from stdin, which blocks to EOF on a tty; run by hand with no
  `--transcript` it simply sat there. It now checks for a terminal first and
  exits with the same message it always intended to print.
- `brain doctor`'s git check leaked a heap allocation per pending file.

## [0.7.0] — 2026-08-27

The **second brain**: a persistent memory that belongs to you and your machine
rather than to a repository. Markdown in a git repo — readable, greppable and
revertible without RagPilot in the picture. The vector collection is a
retrieval layer rebuilt from it, never the only copy of anything.

### Added
- **`ragpilot brain init`** — creates `<data_root>/brain/`
  (`daily/ knowledge/ skills/ inbox/ archive/`), makes it a git repo with a
  first commit, and writes `persona.md` and `config.toml`. An existing brain is
  **upgraded, never wiped**: the schema version is bumped and every setting is
  left as it was.
- **Four MCP tools that work in any folder, project or not** — `brain_load`
  (a token-budgeted opening package: persona, open threads, recent decisions),
  `brain_search`, `brain_note` (searchable within a second) and `brain_flush`.
- **Session hooks.** `SessionStart` / `SessionEnd` / `PreCompact` are installed
  into `.claude/settings.local.json` for Claude Code, so context arrives and
  the session is recorded without the agent choosing to cooperate. Clients
  without lifecycle hooks get the same contract as a convention block in their
  agent markdown.
- **`ragpilot brain compile`** — distils changed logs and inbox drops into
  knowledge notes with frontmatter and wikilinks, drafts `skills/` from repeated
  procedures, and commits. It **never deletes**: conflicting information is
  marked with a `> ⚠ Çelişki:` comment under the old claim. Output that does not
  parse is skipped whole and reported — a chunk is never half-applied.
- **`ragpilot brain import`** — ChatGPT exports, claude.ai exports, Claude Code
  and Codex session logs, and plain markdown. Raw conversations are archived
  verbatim; `--limit` and `--since` make a large archive tractable.
- **`ragpilot brain doctor [--fix]`** — schema, engine, scheduler, wikilinks,
  index drift, orphaned vectors and git state. `--fix` repairs only what is
  derived from the markdown; a broken wikilink is reported with a suggestion and
  left to you. `ragpilot doctor` summarises the brain too.
- **`ragpilot brain schedule`** — a systemd user timer or a launchd agent for
  the daily compile. Always explicit; nothing registers a background job on its
  own.
- **Two compiler engines behind one trait.** `claude-cli` runs on your existing
  Claude subscription with no API key; `gemini-api` uses `GEMINI_API_KEY`, read
  from the environment at call time and never written to a config file.
  `--engine` overrides either for one run.
- `rag_search` gained `scope: project | brain | both`; `context_bundle` gained an
  opt-in `include_brain`. Both default to the previous behaviour.
- **[docs/brain.md](docs/brain.md)** — a setup spec written to be handed to a
  coding agent and executed.

### Security
- **Brain data can never reach a reporting path.** `brain_*` calls are excluded
  from the observation seam before any observer is invoked — enforced in the
  open source rather than promised by a separate build, and covered by a test.
  See [SECURITY_MODEL §10](docs/SECURITY_MODEL.md).

## [0.6.0] — 2026-08-27

**A project folder now keeps nothing but its MCP config and agent markdown.**
Everything RagPilot writes moves to one machine-global data root.

### Added
- **Global data layout** — `~/.local/share/ragpilot/` (override with
  `RAGPILOT_DATA_DIR`) holding `registry.json` and `projects/<id>/`. A project
  id is `<folder-slug>-<blake3 of its canonical path>`, which is also its Qdrant
  collection name.
- **`ragpilot migrate [--keep]`** — moves a `.rag/` project into the global
  layout **without re-indexing**: the existing collection is aliased to the new
  name, so search results are identical either side of it. `--keep` copies
  rather than moves, leaving `.rag/` as a working fallback.
- **`ragpilot projects list | rm <id> | relink <id> <path>`**. `rm` follows an
  alias to the physical collection so a migrated project leaves nothing behind,
  and never touches the project's own files.
- **`ragpilot paths`** — prints the resolved locations for the current project,
  so scripts stop guessing at `.rag/`.
- **Layered configuration**: environment > project config > global config >
  built-in defaults. Tables merge; arrays replace.

### Changed
- `ragpilot init <agent>` no longer creates `.rag/`. It registers the project,
  writes the MCP config, and maintains a marked
  `<!-- ragpilot:start -->` block in the agent markdown — created if absent,
  replaced in place if present, appended below your own text otherwise, and
  never duplicated.
- Un-migrated indexes are protected: if the new collection is missing while the
  old name still holds vectors, indexing stops and points at `migrate` rather
  than quietly building an empty collection beside it.

### Fixed
- `ragpilot doctor` checked `.claude/settings.json` for the MCP registration,
  which `init` has not written for some time — a correctly configured project
  failed the check. It now looks at `.mcp.json`, with the old location as a
  fallback.

### Compatibility
- Projects with a `.rag/` directory keep working for one minor release, with a
  one-line migrate reminder on each command.

### Changed
- **The crate is now a library as well as the `ragpilot` binary.** All CLI
  logic moved into the library (`ragpilot::run`); `main.rs` is a thin entry
  point. This lets a separate distribution reuse the exact same engine instead
  of forking it.
- **Generic observation seam** (`ToolObserver` / `ObserverContext` +
  `run_server_with`). The MCP server can call an optional observer *after* each
  exchange is answered — a neutral extension point with no behaviour of its own.
  The open-source binary ships **no** observer and makes no network calls; a
  separate build can attach one. (The core carries no usage/telemetry code.)

### Security
- **Fixed a path-traversal vulnerability** in the file-serving MCP tools
  (`rag_get_file_ranges`, `rag_get_skeleton`). The previous `starts_with`
  containment check was lexical, so `../../../../etc/passwd` resolved and read
  files outside the project root. Paths now go through `resolve_in_root`, which
  rejects `..` and absolute paths and enforces canonical containment (defeating
  symlink escapes); covered by tests and live exploit re-tests.
- Added **[docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md)** — data-flow
  diagram, trust boundaries, threat table, and a hardening checklist, each
  grounded in the code.

### Added
- **Offline / air-gapped operation** is now first-class: deterministic model
  cache resolution (config `cache_dir` → project `.fastembed_cache` → shared
  `~/.cache/ragpilot/models`), an actionable error when the model is missing,
  a `ragpilot doctor` offline-readiness check, and a README section (verified
  with all network access blocked).

### Fixed
- Config parse errors are surfaced instead of being masked as
  "No .rag/config.toml found".

## [0.5.0] - 2026-07-05

### Added
- **`--version` / `-V` flag** and first **crates.io release**
  (`cargo install ragpilot`).
- **Project-root resolution for folder-independent MCP clients** — the server
  now resolves its project in priority order: an explicit `--root <path>` /
  `RAGPILOT_ROOT` env var, the workspace root the client announces during
  `initialize` (`rootUri` / `workspaceFolders` / `rootPath`), then the working
  directory. Global clients (Antigravity, Windsurf) get `--root`-pinned config
  snippets from `init`.
- **Dart/Flutter** in the `init` language selection; `.dart` files are labeled
  `dart` in search results and language filters.

### Changed
- **Impact analysis is now call-graph-driven.** `impact_analyze` walks
  incoming call edges transitively (BFS with hop distance and `via` chain),
  derives affected files from real callers, matches the import graph with
  language-aware module patterns (Rust `crate::`/`super::`, Python dotted
  modules, JS/TS relative + index imports), skips ambiguous names (multiple
  project-wide definitions) with an explicit signal instead of flooding
  results, and reports real direct-caller counts in `breaking_signals`.
- **`rag_get_file_ranges` resolves symbols through the symbol graph** — exact
  start/end lines for any indexed kind (`const`, `struct`, `pub async fn`, …),
  with a broader qualifier-stripping text fallback.
- **All output is English-only** — generated `AGENTS.md`/`CLAUDE.md` policy
  docs, `doctor` warnings, `init`/`setup` labels, and agent registration
  notices.

### Fixed
- `impact_analyze` returned empty results structurally: imports are stored as
  module paths but were queried with file paths, and the call graph was never
  consulted.
- MCP server startup no longer `exit(1)`s before answering `initialize` when
  no config is found (clients saw `calling "initialize": EOF`); it now answers
  the handshake and reports a clear error on tool calls instead.
- Deleted files no longer leave stale rows in the import-dependency index
  (cleanup on delete + orphan pruning on reindex).

## [0.4.0] - 2026-06-29

### Added
- **MIT `LICENSE` file** and complete Cargo metadata (description, license,
  repository, keywords, categories) — the README already declared MIT, but the
  license file itself was missing.
- **Governance docs**: `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`,
  `ROADMAP.md`.
- **Reproducible benchmark harness** under `benchmark/` (replacing the ad-hoc
  `bench/` scripts): ten scenarios, three runs each, writes `results.json` +
  `report.md` + raw outputs.
- Project logo.

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

<p align="center">
  <img src="https://raw.githubusercontent.com/alikaya/ragpilot/main/logo.png" alt="ragpilot logo" width="200">
</p>

<h1 align="center">ragpilot</h1>

<p align="center">RAG (Retrieval-Augmented Generation) MCP server for local codebases.</p>

<p align="center">
  <a href="https://crates.io/crates/ragpilot"><img src="https://img.shields.io/crates/v/ragpilot.svg" alt="crates.io"></a>
  <a href="https://crates.io/crates/ragpilot"><img src="https://img.shields.io/crates/d/ragpilot.svg" alt="downloads"></a>
  <a href="https://github.com/alikaya/ragpilot/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

Provides tools to AI agents — Claude Code, Codex, Cursor, VS Code, opencode, Antigravity, and Windsurf — that help them understand your project: semantic search, symbol navigation, impact analysis, and context bundling.

---

## Features

- **Semantic search** — Vector-based code search with Qdrant + fastembed
- **Symbol graph** — Function/struct/class definitions, import and call relationships
- **Multi-language parsing** — Tree-sitter symbol & call extraction for Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby and PHP (regex fallback for other languages); queries live in `queries/<lang>/*.scm` and can be overridden per project under `.rag/queries/`
- **Impact analysis** — Show which files would be affected before refactoring
- **Context bundling** — Complete context in a single call with token budgeting
- **Incremental indexing** — Re-index only changed files
- **Real-time watching** — Automatically detect file changes while the MCP server is running
- **Multiple embeddings** — Local (fastembed) or API (OpenAI, Cohere, Jina)
- **Multi-client setup** — One-command MCP registration for Claude Code, Codex, Cursor, VS Code, and opencode (plus paste-in snippets for the global-only Windsurf & Antigravity CLI) via `ragpilot init <dir> <agent>`

---

## 📊 Performance & Token Efficiency

`ragpilot` is built around token budgeting: instead of dumping whole files into the LLM context, it bundles only the chunks a task needs. The numbers below come from a **reproducible benchmark** — [`benchmark/run_benchmark.sh`](benchmark/run_benchmark.sh) — that runs ten scenarios (search, `context_bundle`, symbol navigation, impact analysis, skeletons, incremental indexing) three times each and writes `results.json` + `report.md` + raw outputs. Point it at any indexed project.

Measured on two codebases with the local `bge-small` model:

| Codebase | Files | `context_bundle` saving (aggregate) | Per-task range | 1-file re-index |
| :--- | :--- | :--- | :--- | :--- |
| **ragpilot** (this repo · Rust) | 31 | **6.0x** — 80–85% fewer tokens | 5.2x–6.72x | ~287 ms |
| **NewPortal** (Nuxt + Rust app) | 213 | **9.12x** — 89% fewer tokens | 3.98x–23.85x | ~272 ms |

- **Semantic search** lands on the right file (e.g. a Qdrant query → `src/store/qdrant.rs`, 0.81 cosine).
- **Skeletons** cut large code files by **84–93%** (signatures kept, bodies elided).
- **Incremental indexing** re-embeds only changed files — a no-op scan is ~240 ms.

> 💡 **How to read this:** the full-file baseline is an **upper bound** (the tokens an agent would spend reading every relevant file whole), so absolute ratios are optimistic and shift with codebase size, language mix, and embedding model. Per task the ratio tracks *how much* context a task retrieves, not its label — it is **not strictly monotonic**. See [`benchmark/report.md`](benchmark/report.md) for full methodology and caveats.

---

## Requirements

- Rust 1.75+
- [Qdrant](https://qdrant.tech) vector database

```bash
# Start Qdrant with Docker
docker run -d -p 6334:6334 qdrant/qdrant
```

---

## Installation

### From crates.io

```bash
cargo install ragpilot
```

### From source

```bash
git clone https://github.com/alikaya/ragpilot
cd ragpilot
cargo install --path .
```

---

## Quick Start

```bash
cd /your/project

# Index the project only
ragpilot init

# Index + register the MCP server with a specific agent:
ragpilot init . claude      # Claude Code  → .mcp.json + CLAUDE.md
ragpilot init . codex       # Codex        → .codex/config.toml + AGENTS.md
ragpilot init . cursor      # Cursor       → .cursor/mcp.json + AGENTS.md
ragpilot init . vscode      # VS Code      → .vscode/mcp.json + AGENTS.md
ragpilot init . opencode    # opencode     → opencode.json + AGENTS.md

# Register every supported client at once
ragpilot init . all
```

> `setup` is an alias for `init <folder> <agent>`, so `ragpilot setup . claude` works too.

See [Supported MCP Clients](#supported-mcp-clients) for the full list, including the global-only clients (Windsurf, Antigravity).

### Manual MCP Registration

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "ragpilot": {
      "type": "stdio",
      "command": "ragpilot",
      "args": ["--mcp-server"]
    }
  }
}
```

---

## Supported MCP Clients

`ragpilot init <dir> <agent>` writes the correct config for each client in its own format. Each client discovers MCP servers differently — `ragpilot` handles the per-client root key and entry shape automatically, and migrates any older `rag` entry to `ragpilot`.

| `<agent>` | Config file written | MCP key | Scope |
|-----------|---------------------|---------|-------|
| `claude` | `.mcp.json` + `CLAUDE.md` | `mcpServers` | project |
| `codex` | `.codex/config.toml` + `AGENTS.md` | `[mcp_servers.ragpilot]` | project |
| `cursor` | `.cursor/mcp.json` + `AGENTS.md` | `mcpServers` | project |
| `vscode` | `.vscode/mcp.json` + `AGENTS.md` | `servers` | project |
| `opencode` | `opencode.json` + `AGENTS.md` | `mcp` (command array) | project |
| `windsurf` | `~/.codeium/windsurf/mcp_config.json` | `mcpServers` | **global** |
| `antigravity` | `~/.gemini/config/mcp_config.json` | `mcpServers` | **global** |
| `all` | every project client + both global snippets | — | — |

**Global-only clients** (Windsurf, Antigravity CLI/IDE) keep their config in `$HOME` and would affect every project, so `ragpilot` never writes outside the repo for them — it prints a ready-to-paste snippet and the exact file path instead.

> **Gemini CLI** was retired on 2026-06-18 in favour of the **Antigravity CLI** (binary `agy`). `gemini` is still accepted as a deprecated alias and is redirected to `antigravity`. Antigravity CLI and IDE 2.0 share `~/.gemini/config/mcp_config.json`; the CLI-only path is `~/.gemini/antigravity-cli/mcp_config.json`.

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `ragpilot init <folder> <agent>` | Index + register the MCP server for an agent (`claude` \| `codex` \| `cursor` \| `vscode` \| `opencode` \| `windsurf` \| `antigravity` \| `all`) |
| `ragpilot setup <folder> <agent>` | Alias for `ragpilot init <folder> <agent>` |
| `ragpilot init [--force]` | Index the project for the first time |
| `ragpilot update` | Re-index changed files |
| `ragpilot status` | Show index statistics |
| `ragpilot clean [--yes]` | Delete the Qdrant collection |
| `ragpilot hooks` | Install git `post-commit` / `post-merge` hooks |
| `ragpilot doctor` | Check installation and configuration |
| `ragpilot --mcp-server` | Start the MCP server over stdio |

### Examples

```bash
# Codex setup for a Vue.js project
ragpilot setup /home/user/vueadmin codex

# Setup with Claude Code
ragpilot setup /home/user/api-server claude

# Index only src/ and lib/ directories (.rag/config.toml)
# include_dirs = ["src", "lib"]
ragpilot update
```

---

## MCP Tools

AI agents use these tools automatically:

| Tool | Description |
|------|-------------|
| `rag_search` | Semantic code search (filter by: path, language, extension) |
| `rag_get_chunks` | Retrieve full content by chunk ID |
| `rag_get_file_ranges` | Read specific line ranges or symbol definitions |
| `rag_index_status` | Index status and dirty file count |
| `rag_ensure_index` | Re-index changed files |
| `nav_symbol_resolve` | Symbol definition + call graph |
| `nav_call_graph` | BFS call tree (incoming + outgoing) |
| `impact_analyze` | Pre-refactor impact analysis |
| `context_bundle` | Token-budgeted complete context bundle |

---

## Configuration

The `.rag/config.toml` file is automatically created with `ragpilot init`:

```toml
[project]
name = "my-project"

[embedding]
provider = "local"   # "local" | "api"

[embedding.local]
model = "BAAI/bge-small-en-v1.5"   # dim=384, 130MB

[qdrant]
url = "http://localhost:6334"

[indexing]
chunk_size    = 400
chunk_overlap = 50
include_extensions = ["rs", "py", "ts", "js", "go", "md"]
exclude_dirs  = ["target", "node_modules", ".git"]
# include_dirs = ["src", "lib"]   # if empty, the entire project is indexed

[watcher]
enabled     = true    # Automatically detect changes while MCP is running
debounce_ms = 500

[symbol_graph]
enabled   = true
max_depth = 3         # impact_analyze BFS depth
```

### Supported Embedding Models

| Model | Dimensions | Size |
|-------|------------|------|
| `BAAI/bge-small-en-v1.5` | 384 | 130 MB (default) |
| `BAAI/bge-base-en-v1.5` | 768 | 430 MB |
| `BAAI/bge-large-en-v1.5` | 1024 | 1.2 GB |
| `nomic-ai/nomic-embed-text-v1.5` | 768 | — |

### Offline / Air-Gapped Operation

With the local provider, only the **first** run needs internet — it downloads
the embedding model from huggingface.co into a cache. After that, indexing and
search run fully offline (verified with all network access blocked).

The cache location is resolved in this order:

1. `embedding.local.cache_dir` in `.rag/config.toml` (if set)
2. `<project>/.fastembed_cache/` (if it already exists)
3. `~/.cache/ragpilot/models/` — shared user-level default, so the model is
   downloaded once per machine, not once per project

For a machine with no internet access:

```bash
# On a networked machine (same OS/arch), populate the cache once:
cargo install ragpilot && cd /any/project && ragpilot init

# Copy the cache to the air-gapped machine:
cp -r ~/.cache/ragpilot/models  <air-gapped>:~/.cache/ragpilot/models

# Verify offline readiness on the target machine:
ragpilot doctor    # → "✓ Embedding model cached (…)"
```

Everything else is local by design: Qdrant runs on your own host, the symbol
graph is SQLite on disk, and `ragpilot` never calls a language-model API.

---

## Project Structure

```
src/
  main.rs              CLI dispatcher
  agents.rs            Per-client MCP registration (claude/codex/cursor/vscode/opencode/…)
  wizard.rs            Interactive language/dir detection for init
  config.rs            TOML configuration structs
  indexer.rs           File scanning, chunking, hash detection
  orchestrator.rs      Indexing engine that coordinates all stores
  watcher.rs           Real-time file watcher (notify v6)
  semantic_diff.rs     Symbol-level diff + blast radius (review command)
  skeleton.rs          Token-efficient file skeletons
  tokens.rs            Token estimation
  parser/
    mod.rs             Symbol/import/call data structures
    regex_parser.rs    Language-specific regex parser (fallback)
    tree_sitter_parser.rs  Multi-language tree-sitter parser (tags.scm engine, 11 languages)
  embedder/
    mod.rs             Embedder trait + factory
    local.rs           fastembed wrapper
    api.rs             OpenAI/Cohere/Jina HTTP embedder
  store/
    mod.rs             Chunk, SearchFilters, VectorStore trait
    qdrant.rs          Qdrant implementation
    sqlite.rs          SQLite schema and connection manager
    symbol_graph.rs    Symbol graph store
    project_tree.rs    Project tree store
    impact_index.rs    Reverse dependency store
  mcp/
    mod.rs             stdio JSON-RPC server loop
    protocol.rs        McpRequest / McpResponse
    tools/
      mod.rs           McpContext + dispatch (underscore tool names)
      rag.rs           rag_search / rag_get_chunks / rag_get_file_ranges / rag_get_skeleton
      nav.rs           nav_symbol_resolve / nav_call_graph
      impact.rs        impact_analyze
      context.rs       context_bundle
      index.rs         rag_index_status + rag_ensure_index
      review.rs        review_semantic_diff
queries/               Per-language tree-sitter queries (.scm) — embedded, overridable via .rag/queries/
benchmark/             Reproducible performance / token-efficiency harness (run_benchmark.sh)
```

> **Note on tool names:** MCP tools use underscores (e.g. `rag_search`), not dots. Some clients (Antigravity/Gemini, Copilot, Cursor) reject or silently drop names containing dots. Legacy dotted names are still accepted by the dispatcher for backward compatibility.

---

## Data Storage

```
.rag/
  config.toml    Project configuration
  state.json     File hash table (change detection)
  stores.db      SQLite: symbols, tree, dependencies
```

Qdrant collection: `<project_name>` (lowercase, spaces → `_`)

---

## Development

```bash
# Debug build
cargo build

# Test
cargo test

# Log level
RAG_LOG=debug ragpilot update

# MCP server debug
RAG_LOG=debug ragpilot --mcp-server 2>debug.log
```

---

## License

MIT

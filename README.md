# ragpilot

RAG (Retrieval-Augmented Generation) MCP server for local codebases.

Provides tools to AI agents — Claude Code, Codex, Cursor, VS Code, opencode, Antigravity, and Windsurf — that help them understand your project: semantic search, symbol navigation, impact analysis, and context bundling.

---

## Features

- **Semantic search** — Vector-based code search with Qdrant + fastembed
- **Symbol graph** — Function/struct/class definitions, import and call relationships
- **Impact analysis** — Show which files would be affected before refactoring
- **Context bundling** — Complete context in a single call with token budgeting
- **Incremental indexing** — Re-index only changed files
- **Real-time watching** — Automatically detect file changes while the MCP server is running
- **Multiple embeddings** — Local (fastembed) or API (OpenAI, Cohere, Jina)
- **Multi-client setup** — One-command MCP registration for Claude Code, Codex, Cursor, VS Code, and opencode (plus paste-in snippets for the global-only Windsurf & Antigravity CLI) via `ragpilot init <dir> <agent>`

---

## 📊 Performance & Token Efficiency

`ragpilot` is designed with a strict focus on token budgeting and cost efficiency. Instead of dumping the entire codebase into the LLM context (context bloating), it uses intelligent semantic filtering and impact analysis to bundle only what is strictly necessary.

Here are the **empirical benchmark results** measured using real-world tasks and the `cl100k_base` (tiktoken) tokenizer:

### 1. Baseline Token Footprint
When reading files without context optimization, a typical codebase snapshot quickly exhausts token limits:

| Scope | Token Count (tiktoken) |
| :--- | :--- |
| **4 Key Source Files** | 15,741 tokens |
| **Full `src/` Directory (24 files)** | 38,415 tokens |

### 2. Context Bundling Efficiency (A/B Test)
We simulated **5 distinct coding tasks** (ranging from minor bug fixes to large structural refactoring) comparing standard file dumping against `ragpilot`'s `context_bundle` tool:

| Scenario / Task Scope | Context Reduction (Compression) |
| :--- | :--- |
| **Per-task Average Reduction** | **4.77x fewer tokens** |
| **Total Cumulative Reduction** | **4.86x fewer tokens** |
| **Peak Efficiency** *(Large tasks touching heavy files)* | **7.33x fewer tokens** |
| **Minimum Efficiency** *(Small tasks already isolated to 2 files)* | **1.45x fewer tokens** |

> 💡 **Key Finding:** Token savings are dynamic and scale with the complexity of the query. While minor isolated tasks achieve a steady **1.45x** reduction, complex structural modifications touching multiple subsystems scale up to a **7.33x** reduction in context size. This directly translates to faster AI response times and up to a **70-80% drop in LLM API costs**.

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

```bash
git clone https://github.com/kullanici/ragpilot
cd ragpilot
cargo build --release
sudo cp target/release/ragpilot /usr/local/bin/ragpilot
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
    regex_parser.rs    Language-specific regex parser
    tree_sitter_parser.rs  Tree-sitter Rust parser (regex fallback)
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

# ragpilot — Usage Guide

`ragpilot` is an MCP server that performs RAG (Retrieval-Augmented Generation) on codebases.
It provides tools to Claude Code that understand your local project: semantic search, symbol navigation, impact analysis, and context bundling.

---

## Table of Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [CLI Commands](#cli-commands)
- [Configuration](#configuration)
- [MCP Tools](#mcp-tools)
  - [rag_search](#ragsearch)
  - [rag_get_chunks](#ragget_chunks)
  - [rag_get_file_ranges](#ragget_file_ranges)
  - [rag_index_status](#ragindex_status)
  - [rag_ensure_index](#ragensure_index)
  - [nav_symbol_resolve](#navsymbol_resolve)
  - [nav_call_graph](#navcall_graph)
  - [impact_analyze](#impactanalyze)
  - [context_bundle](#contextbundle)
- [Manual Testing (JSON-RPC)](#manual-testing-json-rpc)
- [Environment Variables](#environment-variables)
- [Troubleshooting](#troubleshooting)

---

## Installation

```bash
# Dependencies: Qdrant vector database
docker run -d -p 6334:6334 qdrant/qdrant

# Build and install
cargo build --release
sudo cp target/release/ragpilot /usr/local/bin/ragpilot

# Register as MCP server for Claude Code
# Add to .claude/settings.json:
```

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

## Quick Start

```bash
cd /your/project

# 1. First time: create the index
ragpilot init

# 2. Start Claude Code — the MCP server activates automatically
# 3. Watch mode (auto-enabled within MCP): changed files are re-indexed instantly

# Subsequent updates
ragpilot update

# Check status
ragpilot status

# Install git hooks (auto-update after commit)
ragpilot hooks

# Check system health
ragpilot doctor
```

---

## CLI Commands

| Command | Description |
|---------|-------------|
| `ragpilot init [--force]` | Indexes the project for the first time. With `--force`, deletes and recreates the existing index. |
| `ragpilot update` | Re-indexes only changed files (hash comparison). |
| `ragpilot status` | Shows index statistics: file count, chunk count, model, Qdrant status. |
| `ragpilot clean [--yes]` | Deletes the Qdrant collection and state.json. With `-y`, skips confirmation. |
| `ragpilot hooks` | Installs git `post-commit` and `post-merge` hooks. |
| `ragpilot doctor` | Checks configuration, Qdrant connection, binary, git hooks, and MCP registration. |
| `ragpilot --mcp-server` | Starts the MCP server over stdio (automatically invoked by Claude Code). |

### `ragpilot init`

```
.rag/
  config.toml   ← configuration (created and populated)
  state.json    ← hash table (change detection)
  stores.db     ← SQLite: symbols, tree, dependencies
```

A `<project_name>` collection is created in Qdrant and all files are indexed.

### `ragpilot hooks`

Does not overwrite existing hooks; appends the `ragpilot update` line at the end:

```sh
#!/bin/sh
# ... existing hook content ...
# ragpilot: auto-reindex on commit
ragpilot update 2>/dev/null || true
```

### `ragpilot doctor` Output

```
─── ragpilot doctor ─────────────────────────────────
  ✓  Config file exists
  ✓  State file exists
  ✓  SQLite stores exist
  ✓  Qdrant reachable (http://localhost:6334)
  ✓  'ragpilot' binary in PATH
  ✓  Git repository
  ✓  Git hooks installed (run 'ragpilot hooks')
  ✓  Claude Code MCP registration (.claude/settings.json)
```

---

## Configuration

Configuration file: `.rag/config.toml`

```toml
[project]
name = "my-project"

# ─── Embedding ───────────────────────────────────────────────────────────────

[embedding]
provider = "local"   # "local" | "api"

[embedding.local]
# Supported models (fastembed 4.x):
#   "BAAI/bge-small-en-v1.5"         → dim=384, fast (130MB)  ← default
#   "BAAI/bge-base-en-v1.5"          → dim=768  (430MB)
#   "BAAI/bge-large-en-v1.5"         → dim=1024 (1.2GB)
#   "nomic-ai/nomic-embed-text-v1.5" → dim=768, long context
model = "BAAI/bge-small-en-v1.5"
# cache_dir = "~/.cache/ragpilot/models"

[embedding.api]
# Activates when provider is changed to "api"
provider = "openai"              # "openai" | "cohere" | "jina"
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"   # environment variable name
batch_size = 32

# ─── Qdrant ──────────────────────────────────────────────────────────────────

[qdrant]
url = "http://localhost:6334"
collection = "my_project"   # default: project.name lowercased
# api_key = "..."           # for Qdrant Cloud

# ─── Indexing ────────────────────────────────────────────────────────────────

[indexing]
chunk_size    = 400   # in characters
chunk_overlap = 50
max_file_size_kb = 500

include_extensions = [
  "rs", "toml", "md", "txt", "json", "yaml", "yml",
  "js", "ts", "jsx", "tsx", "py", "go", "cpp", "c", "h",
  "html", "css", "sh", "sql"
]

exclude_dirs = [
  ".git", ".rag", "target", "node_modules", "__pycache__",
  ".venv", "venv", "dist", "build", ".next", "vendor"
]

# ─── MCP ─────────────────────────────────────────────────────────────────────

[mcp]
context_chunks       = 6      # rag_search default result count
bundle_budget_tokens = 6000   # context_bundle max tokens

# ─── File Watcher ────────────────────────────────────────────────────────────

[watcher]
enabled     = true   # Auto re-index changed files while MCP server is running
debounce_ms = 500    # batch multiple changes after this delay

# ─── Symbol Graph ────────────────────────────────────────────────────────────

[symbol_graph]
enabled   = true   # write symbol, import, and call graph to SQLite
max_depth = 3      # max dependency depth for impact_analyze
```

---

## MCP Tools

The MCP server runs over stdio using the JSON-RPC 2.0 protocol.
Claude Code calls these tools automatically; to test manually, see the [Manual Testing](#manual-testing-json-rpc) section.

---

### `rag_search`

Performs semantic search across the project codebase.

**When to use:** To understand how something works, where it's defined, or to explore the project structure.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `query` | string | ✓ | Natural language search query |
| `k` | integer | — | Number of results to return (default: `context_chunks` from config, typically 6) |
| `filters.path` | string | — | Glob pattern, e.g. `src/**/*.rs` |
| `filters.filetype` | string | — | Extension, e.g. `rs`, `py` |
| `filters.language` | string | — | Language name, e.g. `rust`, `python` |

**Response:** JSON array, each element:

```json
[
  {
    "chunk_id":   "src/main.rs:0",
    "path":       "src/main.rs",
    "score":      0.872,
    "start_line": 1,
    "end_line":   45,
    "language":   "rust",
    "symbol":     "fn main",
    "snippet":    "fn main() -> anyhow::Result<()> {\n    ..."
  }
]
```

`snippet` contains at most 400 characters. Use `rag_get_chunks` for full content.

**Examples:**

```json
{ "query": "how is error handling done" }

{ "query": "authentication middleware", "k": 10 }

{ "query": "database connection", "filters": { "language": "rust" } }

{ "query": "config loading", "filters": { "path": "src/**/*.rs" } }
```

---

### `rag_get_chunks`

Retrieves full content using `chunk_id` values returned from `rag_search`.

**When to use:** When the `rag_search` snippet isn't sufficient; to read the full code block.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `chunk_ids` | string[] | ✓ | `chunk_id` values from the `rag_search` response |
| `max_chars` | integer | — | Maximum characters per chunk (default: 2000) |

**Response:**

```json
[
  {
    "chunk_id":   "src/config.rs:2",
    "path":       "src/config.rs",
    "start_line": 83,
    "end_line":   120,
    "language":   "rust",
    "content":    "pub struct QdrantConfig {\n    pub url: String,\n    ..."
  }
]
```

---

### `rag_get_file_ranges`

Reads specific line ranges or symbol definitions from a file.

**When to use:** To see the full content of a specific function or line range. More efficient than fetching the entire file.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | string | ✓ | File path relative to project root |
| `ranges` | object[] | ✓ | List of ranges (see below) |

Each range is specified either by line numbers or by symbol name:

```json
{ "start_line": 10, "end_line": 50 }
{ "symbol": "parse_config" }
```

**Response:**

```json
[
  {
    "path":       "src/config.rs",
    "start_line": 216,
    "end_line":   280,
    "content":    "impl Config {\n    pub fn load(path: &Path) ..."
  },
  {
    "path":       "src/config.rs",
    "symbol":     "default_template",
    "start_line": 224,
    "end_line":   276,
    "content":    "pub fn default_template(project_name: &str) ..."
  }
]
```

Returns `"error": "not found"` if the symbol is not found.

---

### `rag_index_status`

Returns index statistics and project status.

**Parameters:** None.

**Response:**

```
Project:       my-project
Collection:    my_project
Files indexed: 47
Chunks:        ~235
Model:         BAAI/bge-small-en-v1.5 (local)
Last indexed:  2026-02-28 14:30:00 UTC
Git commit:    a3f9c12
Dirty files:   0
```

If `Dirty files` is greater than 0, call `rag_ensure_index`.

---

### `rag_ensure_index`

Re-indexes changed files.

**When to use:** When files have been edited but the watcher is disabled; or to ensure the index is up-to-date before starting a task.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `force` | boolean | — | Re-index all files from scratch (default: false) |

**Response:**

```json
{
  "dirty_count": 3,
  "indexed":     3,
  "duration_ms": 1240,
  "message":     "Indexed 3 of 3 dirty files in 1240ms"
}
```

---

### `nav_symbol_resolve`

Finds where a symbol (function, struct, class, etc.) is defined and returns call graph edges.

**When to use:** To jump to a function or type definition; to see who calls it / what it calls.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `symbol` | string | ✓ | Symbol name (case-insensitive) |

**Response:**

```json
[
  {
    "symbol":     "compute_hash",
    "kind":       "function",
    "path":       "src/indexer.rs",
    "start_line": 220,
    "end_line":   222,
    "calls":      [
      { "symbol": "md5::compute", "line": 221 }
    ],
    "called_by":  [
      { "symbol": "reindex_file", "path": "src/orchestrator.rs", "line": 80 },
      { "symbol": "process_file", "path": "src/orchestrator.rs", "line": 67 }
    ]
  }
]
```

If multiple symbols share the same name (overloads/different files), all are listed.

---

### `nav_call_graph`

Returns the call graph around a symbol: what it calls (BFS, up to depth) and who calls it (1 hop).

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `symbol` | string | ✓ | Center symbol name |
| `depth` | integer | — | BFS depth for outgoing calls (default: 2) |

**Response:**

```json
{
  "symbol":    "ensure_index_inner",
  "path":      "src/orchestrator.rs",
  "kind":      "function",
  "calls": [
    { "symbol": "scan_files",     "path": "src/indexer.rs",      "line": 12, "call_line": 172 },
    { "symbol": "reindex_file",   "path": "src/orchestrator.rs", "line": 95, "call_line": 213 },
    { "symbol": "compute_hash",   "path": "src/indexer.rs",      "line": 220,"call_line": 207 }
  ],
  "called_by": [
    { "symbol": "ensure_index",               "path": "src/orchestrator.rs", "line": 158 },
    { "symbol": "ensure_index_with_progress", "path": "src/orchestrator.rs", "line": 162 }
  ]
}
```

---

### `impact_analyze`

Calculates which files and symbols would be affected when the given symbols or files are modified.

**When to use:** To understand the impact scope before refactoring; to identify dependencies at risk of breaking.

**Parameters** (at least one required):

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `symbols` | string[] | — | Symbol names to analyze |
| `paths` | string[] | — | File paths to analyze |

**Response:**

```json
{
  "changed_paths": ["src/store/mod.rs"],
  "affected_files": [
    "src/mcp/tools/rag.rs",
    "src/mcp/tools/context.rs",
    "src/orchestrator.rs",
    "src/store/qdrant.rs"
  ],
  "affected_symbols": [
    { "symbol": "search",       "kind": "function", "path": "src/mcp/tools/rag.rs",     "start_line": 71 },
    { "symbol": "bundle",       "kind": "function", "path": "src/mcp/tools/context.rs", "start_line": 24 },
    { "symbol": "reindex_file", "kind": "function", "path": "src/orchestrator.rs",      "start_line": 95 }
  ],
  "breaking_signals": [
    "Changing 'VectorStore' in src/store/mod.rs may break 4 dependent file(s)"
  ]
}
```

`breaking_signals` indicates that modifying public APIs (functions, structs, traits) could break dependent files.

---

### `context_bundle`

Prepares a token-budgeted complete context bundle for a task in a single call.

**When to use:** To efficiently gather all relevant context at the start of a task. Prefer this over calling other tools individually.

**Parameters:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `task` | string | ✓ | Description of the task to be done |
| `budget_tokens` | integer | — | Maximum output token count (default: `bundle_budget_tokens` from config, typically 6000) |

**Response:** A four-section JSON object:

```json
{
  "rag_chunks": [
    {
      "chunk_id": "src/store/qdrant.rs:3",
      "path": "src/store/qdrant.rs",
      "score": 0.891,
      "start_line": 95,
      "end_line": 140,
      "language": "rust",
      "snippet": "pub async fn search(&self, vector: &[f32], ..."
    }
  ],
  "symbols": [
    { "symbol": "QdrantStore", "kind": "struct", "path": "src/store/qdrant.rs", "line": 15 },
    { "symbol": "search",      "kind": "function","path": "src/store/qdrant.rs", "line": 95 }
  ],
  "impact_summary": "3 file(s) depend on the matched code: src/orchestrator.rs, src/mcp/tools/rag.rs, ...",
  "tree_snapshot": [
    "src/store/mod.rs",
    "src/store/qdrant.rs",
    "src/store/sqlite.rs",
    "src/store/symbol_graph.rs"
  ],
  "approx_tokens_used": 1840
}
```

The token budget is divided into four parts:
- 60% → `rag_chunks` (semantic search results)
- Up to 80% → `symbols` (symbols in matched files)
- Up to 90% → `impact_summary` (1-hop dependency summary)
- Up to 100% → `tree_snapshot` (file list in matched directories)

---

## Manual Testing (JSON-RPC)

To test the MCP server from the command line:

```bash
# Important: JSON must be on a single line and end with \n
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}' \
  | ragpilot --mcp-server

# Tool list
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | ragpilot --mcp-server

# rag_index_status
echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"rag_index_status","arguments":{}}}' \
  | ragpilot --mcp-server

# rag_search
echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"rag_search","arguments":{"query":"config loading","k":3}}}' \
  | ragpilot --mcp-server

# rag_search with filters
echo '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rag_search","arguments":{"query":"embed function","filters":{"language":"rust"}}}}' \
  | ragpilot --mcp-server

# nav_symbol_resolve
echo '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nav_symbol_resolve","arguments":{"symbol":"compute_hash"}}}' \
  | ragpilot --mcp-server

# context_bundle
echo '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"context_bundle","arguments":{"task":"Understand the Qdrant search implementation","budget_tokens":4000}}}' \
  | ragpilot --mcp-server
```

**For debug logs:**

```bash
RAG_LOG=debug ragpilot --mcp-server 2>rag-debug.log
```

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RAG_LOG` | Log level: `error`, `warn`, `info`, `debug`, `trace`. Default: `warn`. All logs are written to stderr. |
| `OPENAI_API_KEY` | OpenAI embedding API key (customizable via `embedding.api.api_key_env`) |
| `COHERE_API_KEY` | Cohere embedding API key |
| `JINA_API_KEY` | Jina embedding API key |

---

## Troubleshooting

### `stores.db` tables empty after `ragpilot init`

Rebuild and reinstall the binary:

```bash
cargo build --release
sudo cp target/release/ragpilot /usr/local/bin/ragpilot
ragpilot clean --yes && ragpilot init
```

### Qdrant connection error

```bash
# Is Qdrant running?
docker ps | grep qdrant
# or
curl http://localhost:6334/readyz

# Check the URL in config
cat .rag/config.toml | grep url
```

### Embedding model not downloading

```bash
# Check the model cache directory
ls ~/.cache/huggingface/hub/

# Specify a manual path
# In .rag/config.toml:
# [embedding.local]
# cache_dir = "/alternative/directory"
```

### ✗ marks in `ragpilot doctor` output

```bash
ragpilot doctor   # Which checks failed?

# Common issues:
ragpilot init          # Config or state missing
ragpilot hooks         # Git hooks not installed
# Add MCP registration to .claude/settings.json
```

### Symbols not indexed (nav tools return empty)

Check `symbol_graph.enabled`:

```toml
# .rag/config.toml
[symbol_graph]
enabled = true
```

Then re-index:

```bash
ragpilot init --force
```

### MCP server JSON parse error

Make sure JSON is on a single line and ends with `\n`. Extra `}` or `{` characters cause parse errors:

```bash
# Wrong (extra }} present):
echo '{"method":"tools/call","params":{"name":"rag_index_status","arguments":{}}}'

# Correct:
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"rag_index_status","arguments":{}}}'
```

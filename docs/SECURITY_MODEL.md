# RagPilot Security Model

This document describes what RagPilot does with your data, where trust
boundaries lie, the threats we considered, and how to harden a deployment. It
is written to be verifiable: every claim points at the code that implements it.

RagPilot is a local-first code-intelligence MCP server. Its default
configuration is designed so that **no source code leaves the machine**. One
optional, explicitly-configured feature (API embeddings) changes that, and is
called out clearly below.

- Version: applies to 0.5.0 and later.
- Scope: the `ragpilot` binary (CLI + MCP server). It does **not** cover the
  MCP client you connect (Claude Code, Codex, Cursor, …) or the language model
  that client talks to — those are separate trust domains.

---

## 1. What RagPilot is (and is not)

- It **retrieves and serves code context**. It indexes a repository, builds a
  semantic vector index plus a symbol / import / call graph, and exposes
  focused tools over the Model Context Protocol.
- It **never invokes a language model itself.** There is no LLM call anywhere
  in the codebase; RagPilot only embeds text and serves retrieval results. The
  "AI" is entirely in the client you connect.
- It has **no inbound network surface.** The MCP server speaks JSON-RPC 2.0
  over **stdio** only (`src/mcp/mod.rs` — a stdin/stdout loop). There is no
  `TcpListener`, no bound port, no daemon. It runs as a child process of the
  MCP client and dies with it.

---

## 2. Data flow

```
                        ┌─────────────────────────────────────────────┐
   your repository      │  ragpilot process (local)                   │
   (files on disk)      │                                             │
        │               │   indexer ──chunks──▶ embedder              │
        └──scan(root)───▶│                        │                    │
                        │            ┌───────────┴───────────┐        │
                        │       local (default)         api (opt-in)  │
                        │       fastembed ONNX          POST chunks   │
                        │       on-device               to provider ──┼──▶ 3rd-party
                        │            │                                 │    embedding API
                        │        embeddings                           │    (only if enabled)
                        │            ▼                                 │
                        │   Qdrant (vectors)   SQLite .rag/stores.db   │
                        │   localhost:6334      (symbols/graph/tree)   │
                        │            │                  │              │
                        │            └───── tools ──────┘              │
                        │                     │                        │
                        └─────────────────────┼────────────────────────┘
                                              │ JSON-RPC over stdio
                                              ▼
                                        MCP client (Claude Code / Codex / …)
```

Step by step:

1. **Indexing** (`src/indexer.rs`, `src/orchestrator.rs`) — RagPilot scans
   files under the project root, honouring `include_extensions`,
   `include_dirs`, and `exclude_dirs` from `.rag/config.toml`. Only files that
   match are read.
2. **Embedding** — each chunk is turned into a vector by the configured
   provider:
   - **`local` (default)** — `src/embedder/local.rs`, the `fastembed` ONNX
     runtime, executes fully on-device. After the one-time model download
     (see §4) it needs no network at all. Verified: with all network access
     blocked, indexing and search succeed against a populated cache.
   - **`api` (opt-in)** — `src/embedder/api.rs` POSTs the chunk text to
     `api.openai.com`, `api.cohere.ai`, or `api.jina.ai`. **In this mode your
     source code is sent to that third party.** This is never the default and
     must be turned on in config.
3. **Vector store** — embeddings and chunk payloads go to Qdrant, default
   `http://localhost:6334` (`src/config.rs`). Local by default; if you point
   `qdrant.url` at a remote/Cloud instance, your vectors and chunk text go
   there.
4. **Graph store** — symbols, imports, calls, the project tree, and the
   reverse-dependency index live in SQLite at `.rag/stores.db` on local disk.
5. **Serving** — tools return results to the MCP client over stdio.

The core makes **no outbound calls of its own** — no telemetry, no usage
reporting, no phone-home. (`run_server_with` exposes a neutral observation seam
for a *separate* build to attach behaviour, but the open-source binary ships no
observer.)

---

## 3. Trust boundaries

| Boundary | What crosses it | Default posture |
|---|---|---|
| Repository → ragpilot | File contents (only matched files) | Local, in-process |
| ragpilot → embedding provider | **Nothing** (local) / **chunk text** (api) | Local by default; API is opt-in egress |
| ragpilot → Qdrant | Vectors + chunk payloads | `localhost` by default |
| ragpilot → Hugging Face | Model download request (first run only) | One-time; avoidable via pre-seeded cache (§4) |
| MCP client → ragpilot | Tool calls (queries, file paths, symbols) | stdio; confined to project root (§5) |

The two boundaries that can move data off the machine are **API embeddings**
and a **remote Qdrant** — both are configuration choices, off by default, and
visible in `.rag/config.toml`.

---

## 4. First-run model download (local provider)

The local embedding model (~130 MB) is downloaded once from `huggingface.co`
into a cache, then reused offline forever. Cache resolution is deterministic
(`src/embedder/local.rs::resolve_cache_dir`): `embedding.local.cache_dir` if
set, else `<project>/.fastembed_cache` if present, else the shared
`~/.cache/ragpilot/models`.

For an air-gapped machine, pre-seed the cache from a networked machine of the
same OS/arch (see the README "Offline / Air-Gapped Operation" section).
`ragpilot doctor` reports whether the model is cached at the resolved path, so
offline readiness is checkable before deployment.

---

## 5. Path confinement (file-serving tools)

`rag_get_file_ranges` and `rag_get_skeleton` take a caller-supplied path. All
such paths are resolved through `resolve_in_root` (`src/mcp/tools/mod.rs`),
which:

- treats a leading `/` as project-relative, not filesystem-absolute;
- rejects any `..` component and any OS-absolute / prefixed path, so
  `../../../../etc/passwd` and `src/../../etc/passwd` are refused;
- when the target exists, canonicalises both the target and the root and
  requires containment, which additionally defeats symlink escapes.

This is covered by unit tests (`path_safety_tests`) and confirmed against live
exploit attempts. Files the MCP client can read are therefore confined to the
project root.

---

## 6. Secrets handling

- **Embedding API key** — read from an environment variable named by
  `embedding.api.api_key_env` (default `OPENAI_API_KEY`), **not** stored in
  config (`src/embedder/api.rs`). The key never touches disk via RagPilot.
- **Qdrant Cloud key** — `qdrant.api_key` is an optional config field. If you
  use Qdrant Cloud and set it, it is stored in plaintext in `.rag/config.toml`.
  Prefer a local Qdrant, or supply the key by other means; keep `.rag/` out of
  version control (it is `.gitignore`d by default).
- **Generated indexes** — `.rag/` (SQLite + state) and `.fastembed_cache/` are
  `.gitignore`d so index data and models are never committed.

---

## 7. Threat model

| # | Threat | Vector | Mitigation |
|---|---|---|---|
| T1 | Source code exfiltration to a third party | API embedding provider | Local provider is the default and sends nothing; API mode is opt-in and documented. Air-gapped deployments keep the local provider. |
| T2 | Arbitrary file read outside the project | Malicious/confused MCP tool call with `..` or a symlink | `resolve_in_root` rejects `..`/absolute paths and enforces canonical containment (§5). |
| T3 | Inbound network attack on the server | Any remote client | No listener exists; the server is stdio-only, no bound port. |
| T4 | Secret leakage | API keys on disk or in git | API embedding key comes from env, not config; `.rag/` is `.gitignore`d; Qdrant Cloud key documented as the one plaintext case to avoid. |
| T5 | Supply-chain tampering of the model | First-run download over the network | One-time, over TLS to Hugging Face; pre-seeded caches let you avoid it entirely and verify with `doctor`. Pin/verify the cache in high-assurance environments. |
| T6 | Data at rest exposure | Index DB / vectors readable on disk | Index reflects your source; protect `.rag/` and the Qdrant data dir with normal filesystem permissions and disk encryption. |
| T7 | Stale data revealing deleted code | Index not cleaned on delete | Deletes prune the vector store, symbol graph, project tree, and dependency index; reindex self-heals orphans. |

Out of scope: the security of the MCP client and its language model, the OS,
the Qdrant deployment's own auth/TLS, and physical access to the machine.

---

## 8. Hardening checklist

For a privacy-sensitive or air-gapped deployment:

- [ ] Keep `embedding.provider = "local"` (never `api`) so no code leaves the host.
- [ ] Run Qdrant on `localhost` (or a private, authenticated, TLS endpoint you control); do not use a public Qdrant Cloud instance for sensitive code.
- [ ] Pre-seed the embedding-model cache and verify with `ragpilot doctor`; confirm the machine has no outbound access if it must be air-gapped.
- [ ] Do not set `qdrant.api_key` in config on shared machines; if Qdrant auth is required, restrict file permissions on `.rag/config.toml`.
- [ ] Ensure `.rag/` and `.fastembed_cache/` stay out of version control (default `.gitignore` already does this).
- [ ] Apply filesystem permissions / disk encryption to the repo, `.rag/`, and the Qdrant data directory.
- [ ] Scope `include_dirs` / `exclude_dirs` so secrets directories (`.env`, key material) are never indexed.
- [ ] Keep RagPilot updated; review release notes for security-relevant changes.

---

## 9. Reporting a vulnerability

Please report security issues privately to **alikayaa@gmail.com** rather than
opening a public issue. See [`SECURITY.md`](../SECURITY.md) for the disclosure
process.

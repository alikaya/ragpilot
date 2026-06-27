# RagPilot Roadmap

RagPilot is a token-efficient, local-first code-intelligence layer for AI coding
agents over the open Model Context Protocol (MCP). This roadmap reflects the
direction of the project. It is indicative and may evolve.

## Now (shipped — v0.3.0)

- Semantic code search (Qdrant + fastembed)
- Symbol / import / call graph (tree-sitter, 11 languages; regex fallback)
- Impact analysis and symbol-level semantic diff
- Token-budgeted context bundling
- Incremental indexing + real-time file watcher
- Local and API embeddings (OpenAI / Cohere / Jina)
- Multi-client MCP registration (Claude Code, Codex, Cursor, VS Code, opencode,
  Windsurf, Antigravity) + works with any MCP-capable client
- Reproducible benchmark harness

## M1 — Open, vendor-neutral, local-first foundation

- Governance: CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, ROADMAP, templates
- CI/CD with multi-platform release binaries; crates.io publishing
- Documented, tested **fully-local** reference stack (Ollama + opencode)
- Offline / air-gapped robustness and `doctor` checks
- Hardened benchmark harness + public methodology report

## M2 — Retrieval quality & language coverage

- Retrieval-quality evaluation harness (precision/recall on labelled queries)
- Symbol-aware chunking
- Expanded tree-sitter language coverage + query refinement
- Cross-language symbol/import/call extraction quality fixes

## M3 — Refactor-safety & impact intelligence

- Accurate cross-file call-graph resolution
- `impact_analyze`: transitive blast radius + confidence signals
- `review_semantic_diff`: signature / public-API change detection
- Expanded test suite and correctness fixtures

## M4 — Interoperability, packaging, security & outreach

- Any-MCP-client documentation, presets, minimal open-source reference agent
- Packaging & distribution (static binaries, Nix/AUR/Homebrew, container)
- Documentation site (usage, architecture, privacy/deployment spectrum)
- External security review + threat model
- Dissemination and community onboarding

## Later / ideas

- Additional vector-store backends
- Richer per-language query packs contributed by the community
- IDE-side visualisations of impact analysis

> This roadmap is indicative and may evolve. Feedback and contributions are
> welcome — see [`CONTRIBUTING.md`](CONTRIBUTING.md).

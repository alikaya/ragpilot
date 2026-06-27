# Contributing to RagPilot

Thanks for your interest in contributing! RagPilot is an MIT-licensed,
local-first code-intelligence MCP server. Contributions of all kinds are
welcome: bug reports, documentation, language/query support, and code.

## Getting started

**Requirements**

- Rust 1.75+
- [Qdrant](https://qdrant.tech) vector database (for running/testing the server)

```bash
# Start Qdrant
docker run -d -p 6334:6334 qdrant/qdrant

# Clone and build
git clone https://github.com/alikaya/ragpilot
cd ragpilot
cargo build
```

## Development workflow

```bash
cargo build            # debug build
cargo test             # run the test suite
cargo fmt              # format (rustfmt)
cargo clippy           # lint

# Run the MCP server with debug logging
RAG_LOG=debug ragpilot --mcp-server 2>debug.log
```

Before opening a pull request, please make sure:

1. `cargo fmt` and `cargo clippy` are clean.
2. `cargo test` passes.
3. New behaviour has tests where practical.
4. Commit messages follow the existing style (e.g. `feat(parser): ...`,
   `fix(mcp): ...`, `docs: ...`).

## Adding language support

Tree-sitter queries live in `queries/<lang>/*.scm` and are embedded at build
time. They can be overridden per project under `.rag/queries/`. To add or
improve a language, add/adjust its `.scm` query and include a small fixture so
extraction quality can be verified.

## Reporting bugs

Open an issue with: what you expected, what happened, your OS, Rust version,
the embedding provider (local/api), and a minimal reproduction if possible.

## Proposing changes

For anything larger than a small fix, please open an issue first to discuss the
approach. This avoids duplicated effort and keeps the architecture coherent.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating you are expected to uphold it.

## License

By contributing, you agree that your contributions will be licensed under the
project's [MIT License](LICENSE).

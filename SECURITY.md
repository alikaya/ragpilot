# Security Policy

## Supported versions

RagPilot is under active development. Security fixes are applied to the latest
released version on the `main` branch.

| Version | Supported |
|---------|-----------|
| latest (`main`) | ✅ |
| older releases | ❌ |

## Reporting a vulnerability

Please **do not** open a public issue for security vulnerabilities.

Instead, report them privately by email to **alikayaa@gmail.com** with:

- a description of the issue and its potential impact,
- steps to reproduce (a proof of concept if possible),
- any suggested remediation.

You can expect an initial acknowledgement within **5 business days**. Once the
issue is confirmed, we will work on a fix and coordinate a disclosure timeline
with you. Credit will be given to reporters who wish to be acknowledged.

## Scope and threat model notes

RagPilot is a local-first tool. In its default configuration it:

- indexes source code on the local machine,
- stores data locally (`.rag/` and a local Qdrant collection),
- with the **local** embedding provider, performs no network calls at runtime
  after the one-time embedding-model download.

Relevant areas for security consideration include: handling of untrusted
repository content during parsing/indexing, the MCP stdio interface, and the
optional API embedding providers (which send chunk text to a third-party
service when explicitly configured).

A full data-flow diagram, trust-boundary analysis, threat table, and hardening
checklist are in **[docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md)**.

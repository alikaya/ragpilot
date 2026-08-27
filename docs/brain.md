# RagPilot Brain — setup spec

> **For an agent:** this file is executable. Read it, run the steps against the
> user's machine, and report what happened. Ask before anything destructive.
> **For a human:** hand this file to your coding agent and let it do the work,
> or follow it yourself — the commands are the same either way.

---

## 1. What this installs, and why

RagPilot Brain is a **second brain**: a persistent memory that belongs to you
and your machine rather than to any one repository. Its whole content is
markdown in a git repository — you can read it, grep it, edit it and revert it
without RagPilot in the picture at all.

The problem it solves is that agent sessions have no yesterday. Every new
session starts from nothing: the same context re-explained, the same decisions
re-argued, the same dead end walked into twice. The brain fixes that from both
ends. When a session opens, the agent is handed who you are, what was left
half-done and what was already decided. When it closes, the session is
summarised and written back down.

Markdown is the source of truth; the vector index is only a retrieval layer,
rebuilt from the markdown whenever it needs to be. That ordering is what makes
the brain durable: you can change embedding model, delete the whole index, or
walk away from RagPilot entirely, and your notes are still there — plain files,
in a git repo, on your disk. Nothing here reaches the network except the
compiler call you configure (see §7).

---

## 2. Check the prerequisites

```bash
ragpilot --version            # not installed → cargo install ragpilot
curl -s localhost:6333/healthz  # Qdrant must be reachable
ragpilot brain doctor         # is there already a brain here?
```

- **RagPilot missing** → `cargo install ragpilot`.
- **Qdrant not running** → `docker run -d -p 6333:6333 -p 6334:6334 qdrant/qdrant`.
- **A brain already exists** → do **not** delete it. `ragpilot brain init` is an
  upgrade path: it fills in anything missing and bumps the schema version,
  leaving every setting and every note exactly as they are.

---

## 3. Create the vault

```bash
ragpilot brain init
```

It asks three questions. They are worth answering properly — they end up in
`persona.md`, which is the first thing every session sees:

| Question | What it is for |
|---|---|
| **Name** | What the agent calls itself. |
| **Character / tone** | How it should talk to you. "Direct, no flattery" is a real instruction, not decoration. |
| **What you work on** | The areas the compiler should care about. |

The result is `~/.local/share/ragpilot/brain/`:

```
config.toml     engine, model, schedule, budgets
persona.md      yours to edit; the compiler reads it and never rewrites it
daily/          session logs, one file per day
knowledge/      compiled notes
skills/         procedures worth repeating
inbox/          drop anything here; the compiler digests it
archive/        imported history
```

It is a git repository from the first commit, so every later change is one
`git revert` away.

---

## 4. Wire it into your agent

**Claude Code** — hooks do it for you, with no cooperation required:

```bash
cd /path/to/project
ragpilot brain hooks       # or: ragpilot init . claude
```

This writes `SessionStart`, `SessionEnd` and `PreCompact` into
`.claude/settings.local.json` (local, not committed — a brain is personal).
Opening a session injects the brain's context; closing one summarises the
transcript and writes it to today's log. `PreCompact` runs the same command, so
a long session's record reaches disk even if the context window fills first.

**Everything else** (Codex, Cursor, VS Code, opencode, Windsurf, Antigravity) —
no lifecycle hooks yet, so the same contract goes into the agent's markdown as
a convention:

```bash
ragpilot init . codex      # adds the brain section to AGENTS.md
```

The contract is identical either way: `brain_load` first, `brain_note` when
something is decided, `brain_flush` before the end.

---

## 5. Verify

```bash
ragpilot brain doctor
ragpilot brain session-start     # should print your persona
```

Then, in an agent session, ask it to call `brain_load`. You should get your
persona back. Note something, and check it landed:

```bash
cat ~/.local/share/ragpilot/brain/daily/$(date +%F).md
```

`brain doctor` checks the schema, the compiler engine, the scheduler, wikilinks,
index drift, orphaned vectors and git state. `--fix` repairs the derived ones
(index, orphans, uncommitted changes) and never touches your markdown.

---

## 6. First week

- **Let it run.** The brain is worth something after a few days of sessions,
  not after five minutes.
- **Note decisions as they happen**, not at the end: `brain_note` with
  `kind: "decision"`.
- **Use the inbox.** Drop an invoice, a link dump, a half-written design doc
  into `inbox/` and the compiler will fold it in.
- **Compile.** Nightly by default:
  ```bash
  ragpilot brain schedule --install    # systemd timer or launchd agent
  ragpilot brain compile               # or run it by hand
  ```
  The compiler only ever adds. Conflicting information is marked with a
  `> ⚠ Çelişki:` comment under the old claim — you decide which was wrong.

- **Bring your history in.** Years of conversations already hold most of what
  the brain would otherwise take months to learn:
  ```bash
  ragpilot brain import ~/Downloads/chatgpt-export/conversations.json
  ragpilot brain import ~/Downloads/claude-export.json
  ragpilot brain import ~/.claude/projects --since 2026-01-01
  ragpilot brain import ~/notes            # plain markdown works too
  ```
  Start with `--limit 20` to see what the notes look like before committing to
  a whole archive. Raw conversations are kept verbatim in `archive/takeout/`;
  the distilled notes are an opinion, the archive is the record.

---

## 7. What leaves your machine

| Component | Network |
|---|---|
| The vault (`brain/`) | Never. Plain files on your disk. |
| The index | Never. Your own Qdrant. |
| Embeddings | Local model by default. Only leaves if you configure an API provider. |
| **Compiler** | The one call that goes out. `claude-cli` uses your existing Claude subscription; `gemini-api` uses your `GEMINI_API_KEY`. |

The default setup — local embeddings plus `claude-cli` — sends nothing anywhere
except through the Claude subscription you already have. The Gemini key is read
from the environment and is never written to `config.toml`.

---

## Command reference

| Command | Description |
|---|---|
| `ragpilot brain init [--engine <name>]` | Create or upgrade the vault |
| `ragpilot brain hooks` | Install the Claude Code session hooks here |
| `ragpilot brain compile [--light]` | Distil logs into knowledge notes |
| `ragpilot brain import <path> [--limit N] [--since DATE]` | Import a chat archive |
| `ragpilot brain schedule [--install\|--remove\|--print]` | Daily compile |
| `ragpilot brain doctor [--fix]` | Check the vault, repair what is safe |
| `ragpilot brain index` | Re-index the vault |
| `ragpilot brain session-start` / `session-end` | What the hooks call |

| MCP tool | Description |
|---|---|
| `brain_load` | The session-opening package: persona, open threads, recent decisions |
| `brain_search` | Semantic search over the vault (`knowledge` \| `daily` \| `skills`) |
| `brain_note` | Record one thing, searchable immediately |
| `brain_flush` | Write the session block |

These four work in any folder, project or not.

#!/usr/bin/env python3
"""Brain mini-benchmark: does `brain_load` respect its budget, and does
`brain_search` find what it should?

Both are contract checks rather than speed runs. `brain_load` sits at the start
of every session, so an overrun is a bug the user pays for in every single one;
and a brain that cannot find what it recorded is worse than no brain, because
the agent will confidently assume it never happened.

Runs against a throwaway vault under a temp RAGPILOT_DATA_DIR. It creates a
`ragpilot_brain` collection in the configured Qdrant and deletes it afterwards.

    python3 benchmark/brain_benchmark.py [--bin ./target/debug/ragpilot]
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

# The small budgets matter most: they are where the package has to be
# trimmed, which is where an overrun would happen.
BUDGETS = [40, 80, 150, 300, 1000, 4000]

# Each fact goes into its own daily file, and each query must retrieve it.
FACTS = [
    ("2026-03-01", "decision", "Postgres connection pooling uses PgBouncer in transaction mode.",
     "which pooler do we use for postgres"),
    ("2026-03-02", "decision", "Docker images are built multi-stage and shipped on distroless.",
     "how are container images built"),
    ("2026-03-03", "note", "The staging database is restored from production nightly at 02:00.",
     "when is staging data refreshed"),
    ("2026-03-04", "decision", "Frontend state is Zustand; Redux was dropped for bundle size.",
     "what do we use for frontend state management"),
    ("2026-03-05", "note", "Load tests run with k6, not JMeter, because the scripts are JavaScript.",
     "which load testing tool"),
    ("2026-03-06", "decision", "Secrets live in Vault; nothing is read from environment files in production.",
     "where are production secrets stored"),
    ("2026-03-07", "note", "The mobile team releases on Tuesdays, the web team continuously.",
     "when does mobile release"),
    ("2026-03-08", "decision", "Search is Meilisearch for product text and Qdrant for embeddings.",
     "which search engine for product text"),
]


def count_tokens(text):
    try:
        import tiktoken
        return len(tiktoken.get_encoding("cl100k_base").encode(text))
    except Exception:
        # The binary uses cl100k too; without it this is an approximation and
        # the report says so.
        return max(1, len(text) // 4)


class Mcp:
    """A `ragpilot --mcp-server` process spoken to over stdio."""

    def __init__(self, binary, cwd, env):
        self.proc = subprocess.Popen(
            [binary, "--mcp-server"], cwd=cwd, env=env, text=True, bufsize=1,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self.next_id = 1
        self._send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})
        self.proc.stdout.readline()

    def _send(self, payload):
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def call(self, name, arguments):
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": "tools/call",
                    "params": {"name": name, "arguments": arguments}})
        started = time.monotonic()
        line = self.proc.stdout.readline()
        elapsed_ms = (time.monotonic() - started) * 1000
        result = json.loads(line)["result"]
        return result["content"][0]["text"], result.get("isError", False), elapsed_ms

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


def seed(brain_dir):
    daily = os.path.join(brain_dir, "daily")
    os.makedirs(daily, exist_ok=True)
    for date, kind, text, _ in FACTS:
        with open(os.path.join(daily, f"{date}.md"), "w") as f:
            f.write(f"# {date}\n\n- 10:00 [{kind}] {text}\n")
    # An open-threads section, so the load package has all three parts.
    with open(os.path.join(daily, "2026-03-09.md"), "w") as f:
        f.write("# 2026-03-09\n\n## Session 18:00\n\nWrapped up the search work.\n\n"
                "### Decisions\n\n- Qdrant stays the vector store\n\n"
                "### Open threads\n\n- Meilisearch index needs re-tuning\n- k6 thresholds unset\n")


def drop_collection(qdrant_rest):
    try:
        req = urllib.request.Request(f"{qdrant_rest}/collections/ragpilot_brain", method="DELETE")
        urllib.request.urlopen(req, timeout=5).read()
    except Exception:
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="./target/debug/ragpilot")
    ap.add_argument("--qdrant-rest", default="http://localhost:6333")
    args = ap.parse_args()

    binary = os.path.abspath(args.bin)
    if not os.path.isfile(binary):
        sys.exit(f"binary not found: {binary}")

    data_dir = tempfile.mkdtemp(prefix="ragpilot-brain-bench-")
    workdir = tempfile.mkdtemp(prefix="ragpilot-brain-bench-cwd-")
    env = dict(os.environ, RAGPILOT_DATA_DIR=data_dir)
    exact = True
    try:
        import tiktoken  # noqa: F401
    except Exception:
        exact = False

    try:
        subprocess.run([binary, "brain", "init"], cwd=workdir, env=env,
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True, timeout=600)
        brain_dir = os.path.join(data_dir, "brain")
        seed(brain_dir)
        subprocess.run([binary, "brain", "index"], cwd=workdir, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       check=True, timeout=600)

        mcp = Mcp(binary, workdir, env)

        print("── brain_load budget compliance " + "─" * 24)
        if not exact:
            print("  (tiktoken unavailable — token counts are approximate)")
        load_ok = True
        for budget in BUDGETS:
            text, is_error, ms = mcp.call("brain_load", {"max_tokens": budget})
            used = count_tokens(text)
            within = used <= budget
            load_ok &= within and not is_error
            trimmed = "truncated" in text or "too small" in text
            print(f"  budget {budget:>5} → {used:>5} tokens  {ms:6.0f} ms  "
                  f"{'ok' if within else 'OVER BUDGET'}"
                  f"{'  (trimmed)' if trimmed else ''}")

        print()
        print("── brain_search recall " + "─" * 33)
        hits, latencies = 0, []
        for date, _, _, query in FACTS:
            text, is_error, ms = mcp.call("brain_search", {"query": query, "limit": 3})
            latencies.append(ms)
            found = (not is_error) and f"daily/{date}.md" in text
            hits += found
            print(f"  {'✓' if found else '✗'} {query[:48]:<50} {ms:6.0f} ms")

        mcp.close()

        recall = hits / len(FACTS)
        latencies.sort()
        print()
        print("── summary " + "─" * 45)
        print(f"  load budget compliance: {'PASS' if load_ok else 'FAIL'} ({len(BUDGETS)} budgets)")
        print(f"  search recall@3:        {hits}/{len(FACTS)} ({recall:.0%})")
        print(f"  search latency median:  {latencies[len(latencies) // 2]:.0f} ms")
        print(f"  search latency max:     {latencies[-1]:.0f} ms")
        return 0 if (load_ok and recall >= 0.75) else 1
    finally:
        drop_collection(args.qdrant_rest)
        shutil.rmtree(data_dir, ignore_errors=True)
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())

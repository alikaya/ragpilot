#!/usr/bin/env python3
"""Retrieval-quality benchmark for `rag_search`.

The token benchmark (run_benchmark.sh) says how much context an answer costs;
this one says whether the answer is the right code. Each query names the file
and the symbol a developer asking it would want. The query set is written
before a change and frozen, so a later run compares like with like.

    retrieval_eval.py --bin target/release/ragpilot --root <indexed project> \
                      --queries benchmark/retrieval/ragpilot.json [--out result.json]

Symbol spans come from the project's own symbol graph (stores.db), which the
chunker does not touch, so the target does not move between runs.

Metrics, over the top-k results:
  file@1 / file@4 / file@10   a result comes from the expected file
  sym@1  / sym@4  / sym@10    a result's lines overlap the expected symbol
  mrr                          1 / rank of the first symbol hit (0 if none)
4 is the number of results an agent gets when it does not ask for more.
"""
import argparse, json, os, sqlite3, subprocess, sys

KS = (1, 4, 10)


def stores_db(root):
    out = subprocess.run([ARGS.bin, "paths"], cwd=root, capture_output=True, text=True, check=True).stdout
    data_dir = dict(l.split("=", 1) for l in out.splitlines() if "=" in l)["data_dir"]
    return os.path.join(data_dir, "stores.db")


def spans(db, path, name):
    con = sqlite3.connect(db)
    rows = con.execute("SELECT start_line, end_line FROM symbols WHERE path=? AND name=?", (path, name)).fetchall()
    con.close()
    return rows


class Server:
    """One MCP server for the whole run: the model loads once."""

    def __init__(self, root):
        self.p = subprocess.Popen([ARGS.bin, "--mcp-server", "--root", root], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.n = 0
        self.call("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                 "clientInfo": {"name": "retrieval-eval", "version": "0"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def call(self, method, params):
        self.n += 1
        self.send({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        while True:
            msg = json.loads(self.p.stdout.readline())
            if msg.get("id") == self.n:
                return msg

    def search(self, query, k):
        msg = self.call("tools/call", {"name": "rag_search", "arguments": {"query": query, "k": k}})
        text = msg["result"]["content"][0]["text"]
        start, end = text.find("["), text.rfind("]")
        return json.loads(text[start:end + 1]) if start >= 0 else []

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def main():
    spec = json.load(open(ARGS.queries))
    db = stores_db(ARGS.root)
    server = Server(ARGS.root)
    rows = []
    for q in spec["queries"]:
        want = spans(db, q["file"], q["symbol"])
        if not want:
            sys.exit(f"no symbol {q['symbol']} in {q['file']} — fix the query set")
        hits = server.search(q["q"], max(KS))
        file_rank = sym_rank = None
        for rank, h in enumerate(hits, 1):
            if h["path"] != q["file"]:
                continue
            file_rank = file_rank or rank
            if sym_rank is None and any(h["start_line"] <= e and h["end_line"] >= s for s, e in want):
                sym_rank = rank
        rows.append({"q": q["q"], "kind": q.get("kind", "nl"), "file": q["file"], "symbol": q["symbol"],
                     "file_rank": file_rank, "sym_rank": sym_rank,
                     "top": [f'{h["path"]}:{h["start_line"]}-{h["end_line"]}' for h in hits[:4]],
                     "avg_chunk_lines": sum(h["end_line"] - h["start_line"] + 1 for h in hits) / max(len(hits), 1)})
    server.close()

    def summary(sel):
        n = len(sel) or 1
        s = {"n": len(sel)}
        for k in KS:
            s[f"file@{k}"] = round(sum(1 for r in sel if r["file_rank"] and r["file_rank"] <= k) / n, 3)
            s[f"sym@{k}"] = round(sum(1 for r in sel if r["sym_rank"] and r["sym_rank"] <= k) / n, 3)
        s["mrr"] = round(sum(1 / r["sym_rank"] for r in sel if r["sym_rank"]) / n, 3)
        s["avg_chunk_lines"] = round(sum(r["avg_chunk_lines"] for r in sel) / n, 1)
        return s

    result = {"corpus": spec.get("corpus"), "bin": ARGS.bin,
              "all": summary(rows),
              "by_kind": {k: summary([r for r in rows if r["kind"] == k]) for k in sorted({r["kind"] for r in rows})},
              "queries": rows}
    out = json.dumps(result, indent=2, ensure_ascii=False)
    if ARGS.out:
        open(ARGS.out, "w").write(out + "\n")
    print(json.dumps({"all": result["all"], "by_kind": result["by_kind"]}, indent=2))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--queries", required=True)
    ap.add_argument("--out")
    ARGS = ap.parse_args()
    main()

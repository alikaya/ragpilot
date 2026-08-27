#!/usr/bin/env python3
"""Verify a migration: every registered project resolves, has its data, owns its
index alone, and can actually answer a search.

Read-only. Run it after `ragpilot migrate --all`, or any time you want to know
the fleet is healthy.

    python3 scripts/verify_migration.py [--sample 12] [--qdrant http://localhost:6333]
"""
import argparse, collections, json, os, random, subprocess, sys, urllib.request

HOME = os.path.expanduser("~")
SEARCH = ('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n'
          '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"rag_search",'
          '"arguments":{"query":"the main entry point of this project","k":1}}}\n')


def get(url):
    return json.load(urllib.request.urlopen(url, timeout=10))["result"]


def short(p):
    return p.replace(HOME, "~")


def paths_of(binary):
    out = subprocess.run([binary, "paths"], capture_output=True, text=True, timeout=60).stdout
    return dict(l.split("=", 1) for l in out.strip().splitlines() if "=" in l)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sample", type=int, default=12, help="how many projects to search in")
    ap.add_argument("--qdrant", default="http://localhost:6333")
    ap.add_argument("--bin", default="ragpilot")
    args = ap.parse_args()

    env = paths_of(args.bin)
    data_root = env.get("data_root", f"{HOME}/.local/share/ragpilot")
    registry_path = env.get("registry", f"{data_root}/registry.json")

    try:
        projects = json.load(open(registry_path))["projects"]
    except OSError:
        sys.exit(f"no registry at {registry_path} — nothing migrated yet")

    cols = {c["name"] for c in get(f"{args.qdrant}/collections")["collections"]}
    aliases = {a["alias_name"]: a["collection_name"] for a in get(f"{args.qdrant}/aliases")["aliases"]}

    print(f"registry {short(registry_path)}: {len(projects)} project(s)")
    print(f"qdrant   {args.qdrant}: {len(cols)} collection(s), {len(aliases)} alias(es)\n")

    problems = collections.defaultdict(list)
    owners = collections.defaultdict(list)

    for path, entry in sorted(projects.items()):
        pid = entry["id"]
        data = os.path.join(data_root, "projects", pid)

        if not os.path.isdir(path):
            problems["project folder is gone"].append(f"{pid}  {short(path)}")
        missing = [f for f in ("config.toml", "state.json", "stores.db")
                   if not os.path.exists(os.path.join(data, f))]
        if missing:
            problems["missing data files"].append(f"{pid}  {', '.join(missing)}")

        physical = aliases.get(pid, pid if pid in cols else None)
        if physical is None or physical not in cols:
            problems["no collection"].append(f"{pid}  {short(path)}")
            continue
        owners[physical].append(pid)
        try:
            if get(f"{args.qdrant}/collections/{pid}")["points_count"] == 0:
                problems["collection is empty"].append(f"{pid}  {short(path)}")
        except Exception as e:
            problems["collection unreadable"].append(f"{pid}  {e}")

    # The one that would be silent and destructive: two projects, one index.
    for physical, ids in owners.items():
        if len(ids) > 1:
            problems["SHARING ONE INDEX"].append(f"{physical} ← {', '.join(ids)}")

    # Does search actually answer?
    pool = [(p, e) for p, e in sorted(projects.items()) if os.path.isdir(p)]
    random.seed(7)
    sample = random.sample(pool, min(args.sample, len(pool)))
    working = stale = 0
    print(f"searching in {len(sample)} project(s)…")
    for path, entry in sample:
        try:
            out = subprocess.run([args.bin, "--mcp-server"], input=SEARCH, cwd=path,
                                 capture_output=True, text=True, timeout=300)
            line = [l for l in out.stdout.strip().splitlines() if l.strip()][-1]
            result = json.loads(line)["result"]
            text = result["content"][0]["text"].strip()
            if result.get("isError") or not text.startswith("["):
                problems["search failed"].append(f"{entry['id']}  {text[:60]}")
                continue
            hits, end = json.JSONDecoder().raw_decode(text)
            working += bool(hits)
            stale += bool(text[end:].strip())
            if not hits:
                problems["search returned nothing"].append(f"{entry['id']}  {short(path)}")
        except Exception as e:
            problems["search failed"].append(f"{entry['id']}  {type(e).__name__}: {e}")
    print(f"  {working}/{len(sample)} answered · {stale} have a stale index "
          f"(files changed since indexing — run `ragpilot update` there)\n")

    # Collections nothing claims: usually projects that were deleted.
    unclaimed = cols - set(aliases.values()) - set(owners)
    if unclaimed:
        print(f"unclaimed collections: {len(unclaimed)} — from projects that no longer exist.")
        print(f"  {', '.join(sorted(unclaimed)[:12])}{' …' if len(unclaimed) > 12 else ''}")
        print("  Delete one with: ragpilot projects rm <id>, or leave them.\n")

    fatal = {"project folder is gone", "missing data files", "no collection",
             "SHARING ONE INDEX", "search failed", "search returned nothing"}
    if not problems:
        print("✓ everything checks out")
        return 0
    for kind, items in problems.items():
        mark = "✗" if kind in fatal else "!"
        print(f"{mark} {kind}: {len(items)}")
        for i in items[:10]:
            print(f"    {i}")
    return 1 if fatal & problems.keys() else 0


if __name__ == "__main__":
    sys.exit(main())

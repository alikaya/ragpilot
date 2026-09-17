# Retrieval quality

`run_benchmark.sh` measures what an answer costs in tokens. This measures
whether `rag_search` returns the right code at all.

Each query names the file and the symbol a developer asking it would want.
A result counts as a hit when it comes from that file (`file@k`) or when its
lines overlap the symbol (`sym@k`); `mrr` is the mean of 1/rank of the first
symbol hit. `k = 4` is what an agent gets when it does not ask for more.

```bash
# Index a copy of this repo in isolation, src/ only
export RAGPILOT_DATA_DIR=$(mktemp -d)
git archive --prefix=corpus/ HEAD | tar -x -C "$RAGPILOT_DATA_DIR"
(cd "$RAGPILOT_DATA_DIR/corpus" && rm -rf benchmark && ragpilot init)   # pick src when asked

python3 benchmark/retrieval_eval.py --bin "$(command -v ragpilot)" \
  --root "$RAGPILOT_DATA_DIR/corpus" --queries benchmark/retrieval/ragpilot.json
```

Compare two builds on the same frozen queries, and look at the per-query
ranks in `--out`, not only at the averages: with ~40 queries one query moves a
score by about 2.5 points.

## Baseline — 0.10.0, 700-character chunks, bge-small-en-v1.5

| file@1 | sym@1 | sym@4 | sym@10 | mrr |
|---|---|---|---|---|
| 0.846 | 0.487 | 0.821 | 0.949 | 0.636 |

## What has been tried

**Symbol-aligned chunks and a context header (2026-09-17) — not adopted.**
Chunks cut to tree-sitter symbol boundaries, and a `File / In / Defines`
preamble embedded ahead of each chunk, were measured against this set and a
37-query PHP set. Averages improved with the header (MRR 0.568 → 0.619 across
both), but per query 17 improved and 20 got worse; sign test p = 0.74, and the
95% interval of the MRR change was [−0.023, +0.129]. Symbol-aligned chunks
alone helped the PHP set and hurt this one. Larger chunks (1500 characters)
doubled what an agent reads for a gain inside the noise.

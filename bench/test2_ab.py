"""TEST 2 — Eşleştirilmiş A/B görev testi.

Her görev iki yolla "bağlam toplanır" ve LLM'e giren token ölçülür:

  Kol A (klasik):  görevle ilgili dosyaları KOMPLE okumak.
  Kol B (RagPilot): canonical RAG yolu = context.bundle çağrısı (CLAUDE.md
                    politikası: önce context.bundle).

Aynı tokenizer (tiktoken) ile sayılır → oran adildir.

Çalıştırma (sunucu indexli olmalı; gerekirse önce `rag update`):
  uv run --with tiktoken python bench/test2_ab.py
"""
from common import count_tokens, file_tokens
from mcp_client import RagMcp

# Her görev: doğal dil sorusu + klasik yaklaşımda okunacak dosya seti.
TASKS = [
    {
        "q": "ensure_index nasıl çalışır, dirty dosyalar nasıl tespit edilir?",
        "files": ["src/orchestrator.rs", "src/indexer.rs", "src/mcp/tools/index.rs"],
    },
    {
        "q": "context.bundle token tasarrufunu nasıl hesaplıyor?",
        "files": ["src/mcp/tools/context.rs", "src/indexer.rs"],
    },
    {
        "q": "Yeni bir embedder backend nasıl eklenir?",
        "files": ["src/embedder/mod.rs", "src/embedder/local.rs",
                  "src/embedder/api.rs", "src/config.rs"],
    },
    {
        "q": "Sembol çağrı grafiği nasıl parse edilip saklanıyor?",
        "files": ["src/store/symbol_graph.rs", "src/parser/regex_parser.rs",
                  "src/store/sqlite.rs"],
    },
    {
        "q": "Impact analizi hangi tabloyu ve sorguyu kullanıyor?",
        "files": ["src/store/impact_index.rs", "src/mcp/tools/impact.rs"],
    },
]

BUDGET = 6000


def main():
    print("=" * 72)
    print("TEST 2 — A/B görev testi (Kol A: dosya okuma | Kol B: context.bundle)")
    print("=" * 72)

    mcp = RagMcp()
    rows = []
    try:
        for i, t in enumerate(TASKS, 1):
            # Kol A: dosyaları komple oku
            a_tokens = sum(file_tokens(f) for f in t["files"])
            # Kol B: context.bundle
            bundle_text = mcp.call_tool("context.bundle",
                                        {"task": t["q"], "budget_tokens": BUDGET})
            b_tokens = count_tokens(bundle_text)
            ratio = a_tokens / b_tokens if b_tokens else float("inf")
            rows.append((i, t["q"], len(t["files"]), a_tokens, b_tokens, ratio))

            print(f"\n[{i}] {t['q']}")
            print(f"    Kol A ({len(t['files'])} dosya komple): {a_tokens:>7,} token")
            print(f"    Kol B (context.bundle)         : {b_tokens:>7,} token")
            print(f"    → Daralma: {ratio:.2f}x")
    finally:
        mcp.close()

    print("\n" + "=" * 72)
    print("ÖZET")
    print("=" * 72)
    print(f"  {'#':<3}{'Görev':<42}{'A':>8}{'B':>8}{'Oran':>8}")
    tot_a = tot_b = 0
    for i, q, _, a, b, r in rows:
        tot_a += a
        tot_b += b
        short = (q[:39] + "…") if len(q) > 40 else q
        print(f"  {i:<3}{short:<42}{a:>8,}{b:>8,}{r:>7.2f}x")
    print("  " + "-" * 66)
    agg = tot_a / tot_b if tot_b else float("inf")
    print(f"  {'TOPLAM':<45}{tot_a:>8,}{tot_b:>8,}{agg:>7.2f}x")
    ratios = [r for *_, r in rows]
    print(f"\n  Görev-başı ortalama daralma : {sum(ratios)/len(ratios):.2f}x")
    print(f"  Toplam-bazlı daralma        : {agg:.2f}x")
    print("\nNot: Kol A = dosyaları KOMPLE okuma (üst sınır/yaygın anti-pattern).")
    print("Bir mühendis dosyanın bir kısmını okusa A düşer; yine de RAG hedefli kalır.")


if __name__ == "__main__":
    main()

"""TEST 1 — Tiktoken ile kesin baseline.

Amaç: 'byte/4' tahminini gerçek tokenizer sayımıyla değiştirmek.
Klasik (dosya okuma) yaklaşımının kesin token tabanını çıkarır:
  - Bir mimari soru için okunacak 4 anahtar dosya
  - Tüm src/ taraması (geniş-tarama anti-pattern'i)

Çalıştırma:
  uv run --with tiktoken python bench/test1_baseline.py
"""
from common import all_source_files, count_tokens, file_tokens, ROOT

KEY_FILES = [
    "src/indexer.rs",
    "src/main.rs",
    "src/orchestrator.rs",
    "src/mcp/tools/context.rs",
]


def row(label, tokens, lines=None):
    line_str = f"{lines:>6}" if lines is not None else "     -"
    print(f"  {label:<34} {line_str} satır   {tokens:>8,} token")


def main():
    print("=" * 64)
    print("TEST 1 — Kesin token baseline (tiktoken / cl100k_base)")
    print("=" * 64)

    print("\n[A] Mimari sorusu — okunacak 4 anahtar dosya:")
    total_key = 0
    for f in KEY_FILES:
        text = (ROOT / f).read_text(encoding="utf-8", errors="replace")
        tok = count_tokens(text)
        total_key += tok
        row(f, tok, text.count("\n") + 1)
    print("  " + "-" * 58)
    row("ARA TOPLAM (4 dosya)", total_key)

    print("\n[B] Geniş tarama — tüm src/*.rs:")
    files = all_source_files()
    total_all = 0
    for f in files:
        total_all += file_tokens(f)
    row(f"TOPLAM ({len(files)} dosya)", total_all)

    print("\n" + "=" * 64)
    print("BASELINE ÖZET (klasik / dosya-okuma maliyeti)")
    print("=" * 64)
    print(f"  4 anahtar dosya : {total_key:>8,} token")
    print(f"  tüm src/        : {total_all:>8,} token")
    print("\nNot: tiktoken bir proxy'dir; Test 2'deki RAG kolu da AYNI")
    print("tokenizer ile sayılır, dolayısıyla oran karşılaştırması adildir.")

    # Test 2'nin kullanması için makine-okunur çıktı
    import json
    out = {"key_files": total_key, "all_src": total_all,
           "per_key_file": {f: count_tokens((ROOT / f).read_text(errors="replace"))
                            for f in KEY_FILES}}
    (ROOT / "bench" / "baseline.json").write_text(json.dumps(out, indent=2))
    print("\n→ bench/baseline.json yazıldı.")


if __name__ == "__main__":
    main()

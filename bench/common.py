"""Ortak yardımcılar: tokenizer ve dosya okuma.

tiktoken (cl100k_base) bir PROXY tokenizer'dır — Claude'un gerçek tokenizer'ı
değildir, ancak iki kol da (dosya-okuma vs RAG) aynı tokenizer ile sayıldığı
için ORAN (ratio) güvenilirdir. Mutlak token sayıları ±%10-15 sapabilir.

Çalıştırma:  uv run --with tiktoken python bench/testX.py
"""
from pathlib import Path

import tiktoken

ROOT = Path(__file__).resolve().parent.parent
_ENC = tiktoken.get_encoding("cl100k_base")


def count_tokens(text: str) -> int:
    return len(_ENC.encode(text, disallowed_special=()))


def file_tokens(rel_path: str) -> int:
    """Bir dosyanın tamamını okumanın token maliyeti (klasik yaklaşım)."""
    p = ROOT / rel_path
    if not p.exists():
        raise FileNotFoundError(rel_path)
    return count_tokens(p.read_text(encoding="utf-8", errors="replace"))


def all_source_files(glob: str = "src/**/*.rs"):
    return sorted(str(p.relative_to(ROOT)) for p in ROOT.glob(glob))

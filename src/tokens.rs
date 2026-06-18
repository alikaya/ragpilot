//! Shared token estimation using the cl100k_base (tiktoken) tokenizer —
//! the same tokenizer the saving metrics use, so counts are comparable
//! across tools (context.bundle, rag.get_skeleton, ...).

use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

fn bpe() -> Option<&'static CoreBPE> {
    static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
    CL100K.get_or_init(|| tiktoken_rs::cl100k_base().ok()).as_ref()
}

/// Estimate the token count of `text`. Falls back to a chars/4 heuristic
/// if the tokenizer fails to load.
pub fn estimate(text: &str) -> usize {
    if let Some(b) = bpe() {
        return b.encode_with_special_tokens(text).len();
    }
    (text.chars().count() as f64 / 4.0).ceil() as usize
}

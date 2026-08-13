//! Tokenization port (ARCHITECTURE.md §7.5). Token budgets are counted,
//! never estimated — but before the model tokenizer is loaded, the chars/4
//! heuristic is the fallback.

use crate::error::BrainError;

/// Counts and truncates text by model tokens. The real implementation comes
/// from the loaded model (M7.1); before that, [`CharTokenizer`] is the
/// fallback (§7.5).
pub trait TokenizerPort: Send + Sync {
    /// Exact token count for `text`.
    fn count(&self, text: &str) -> usize;

    /// Truncate `text` to at most `max_tokens` tokens, preserving the prefix.
    fn truncate_to(&self, text: &str, max_tokens: usize) -> String;

    /// Model identifier (e.g. `"qwen2.5"` or `"char-fallback"`).
    fn id(&self) -> &str;
}

/// Fallback tokenizer using the chars/4 heuristic. Off by roughly fivefold on
/// CJK (F27) but correct within an order of magnitude for Latin scripts.
/// Replaced by the model tokenizer once the model is loaded (M7.1).
#[derive(Debug, Clone, Default)]
pub struct CharTokenizer;

impl TokenizerPort for CharTokenizer {
    fn count(&self, text: &str) -> usize {
        (text.chars().count() / 4).max(1)
    }

    fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
        let max_chars = max_tokens.saturating_mul(4);
        text.chars().take(max_chars).collect()
    }

    fn id(&self) -> &str {
        "char-fallback"
    }
}

/// Error helper for tokenizers.
pub fn tokenizer_err(msg: impl Into<String>) -> BrainError {
    BrainError::Config(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_tokenizer_count() {
        let t = CharTokenizer;
        assert_eq!(t.count(""), 1); // .max(1)
        assert_eq!(t.count("hello world!"), 3); // 12 chars / 4 = 3
        assert_eq!(t.count("ab"), 1); // 2 chars / 4 = 0 → max(1) = 1
    }

    #[test]
    fn char_tokenizer_truncate() {
        let t = CharTokenizer;
        let text = "abcdefghij"; // 10 chars
        let trunc = t.truncate_to(text, 1); // 1 token = 4 chars
        assert_eq!(trunc, "abcd");
    }

    #[test]
    fn char_tokenizer_id() {
        assert_eq!(CharTokenizer.id(), "char-fallback");
    }
}

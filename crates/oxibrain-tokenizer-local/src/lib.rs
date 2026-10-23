//! Loads an existing tokenizer.json; never downloads artifacts or loads weights.
use oxibrain_ports::tokenizer::TokenizerQuality;
use oxibrain_ports::{BrainError, TokenizerPort};

pub struct FileTokenizer {
    tokenizer: tokenizers::Tokenizer,
    id: String,
    digest: String,
}
impl FileTokenizer {
    pub fn open(path: &std::path::Path) -> Result<Self, BrainError> {
        let bytes = std::fs::read(path)
            .map_err(|e| BrainError::TokenizerUnavailable(format!("tokenizer artifact: {e}")))?;
        let tokenizer = tokenizers::Tokenizer::from_bytes(&bytes)
            .map_err(|e| BrainError::TokenizerUnavailable(format!("tokenizer artifact: {e}")))?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        Ok(Self {
            tokenizer,
            id: format!("tokenizer-json:{digest}"),
            digest,
        })
    }
}
impl TokenizerPort for FileTokenizer {
    fn count(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.len())
            .unwrap_or(usize::MAX)
    }
    fn count_exact(&self, text: &str) -> Result<Option<usize>, BrainError> {
        self.tokenizer
            .encode(text, false)
            .map(|e| Some(e.len()))
            .map_err(|e| BrainError::TokenizerUnavailable(e.to_string()))
    }
    fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
        let Ok(encoded) = self.tokenizer.encode(text, false) else {
            return String::new();
        };
        self.tokenizer
            .decode(&encoded.get_ids()[..encoded.len().min(max_tokens)], false)
            .unwrap_or_default()
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn quality(&self) -> TokenizerQuality {
        TokenizerQuality::Exact
    }
    fn artifact_digest(&self) -> Option<&str> {
        Some(&self.digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_artifact_count_is_independent_of_an_inference_model() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("tokenizer.json");
        std::fs::write(&path,r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0,"hello":1,"세계":2},"unk_token":"[UNK]"}}"#).unwrap();
        let t = FileTokenizer::open(&path).unwrap();
        assert_eq!(t.count_exact("hello 세계").unwrap(), Some(2));
        assert_eq!(t.count_exact("").unwrap(), Some(0));
        assert_eq!(
            t.artifact_digest().unwrap(),
            blake3::hash(&std::fs::read(path).unwrap())
                .to_hex()
                .as_str()
        );
        assert!(t.count(&t.truncate_to("hello 세계", 1)) <= 1);
    }
    #[test]
    fn missing_or_invalid_artifact_is_a_typed_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            FileTokenizer::open(&d.path().join("missing")),
            Err(BrainError::TokenizerUnavailable(_))
        ));
        let p = d.path().join("invalid");
        std::fs::write(&p, "{}").unwrap();
        assert!(matches!(
            FileTokenizer::open(&p),
            Err(BrainError::TokenizerUnavailable(_))
        ));
    }
}

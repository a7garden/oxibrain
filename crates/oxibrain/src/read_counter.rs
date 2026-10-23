//! Converts a fallible exact tokenizer into the conservative pure-packer port.
//! Any failed attempted count invalidates the whole receipt, even if that unit
//! did not fit and the final selected context happens to tokenize successfully.
use oxibrain_ports::{BrainError, TokenizerPort};
pub(crate) struct ReadCounter<'a> {
    inner: &'a dyn TokenizerPort,
    error: std::sync::Mutex<Option<String>>,
}
impl<'a> ReadCounter<'a> {
    pub fn new(inner: &'a dyn TokenizerPort) -> Self {
        Self {
            inner,
            error: std::sync::Mutex::new(None),
        }
    }
    pub fn check(&self) -> Result<(), BrainError> {
        if let Some(message) = self.error.lock().expect("read counter lock").as_ref() {
            return Err(BrainError::TokenizerUnavailable(message.clone()));
        }
        Ok(())
    }
}
impl TokenizerPort for ReadCounter<'_> {
    fn count(&self, text: &str) -> usize {
        match self.inner.count_exact(text) {
            Ok(Some(n)) => n,
            Ok(None) => self.inner.count(text),
            Err(error) => {
                *self.error.lock().expect("read counter lock") = Some(error.to_string());
                usize::MAX
            }
        }
    }
    fn truncate_to(&self, text: &str, n: usize) -> String {
        self.inner.truncate_to(text, n)
    }
    fn id(&self) -> &str {
        self.inner.id()
    }
}

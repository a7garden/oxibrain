//! One process-wide backend shared by local inference and embedding adapters.
use llama_cpp_2::llama_backend::LlamaBackend;
use oxibrain_ports::BrainError;
use std::sync::OnceLock;

pub fn backend() -> Result<&'static LlamaBackend, BrainError> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            if !std::env::var("OXIBRAIN_VERBOSE").is_ok_and(|v| v == "1") {
                llama_cpp_2::send_logs_to_tracing(
                    llama_cpp_2::LogOptions::default().with_logs_enabled(false),
                );
            }
            LlamaBackend::init().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| BrainError::Model(format!("llama.cpp backend initialization: {e}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn repeated_and_concurrent_initialization_shares_one_backend() {
        let first = super::backend().expect("backend") as *const _ as usize;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    super::backend().expect("shared backend") as *const _ as usize
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().expect("worker"), first);
        }
    }
}

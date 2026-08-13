//! Integration test for oxibrain-llm-local against a real GGUF model.
//!
//! Ignored by default — requires a model at `~/.oxi/models/`. Run with:
//!   cargo test -p oxibrain-llm-local --test local_model -- --ignored
//! The Qwen2.5-1.5B-Instruct-Q4_K_M model validates generation + tokenizer.

use oxibrain_llm_local::{LocalLlm, LocalLlmOptions};
use oxibrain_ports::{LlmPort, LlmRequest, TokenizerPort};
use std::path::PathBuf;

fn model_path() -> PathBuf {
    let mut p = home_dir();
    p.push(".oxi/models/qwen2.5-1.5b-instruct-q4_k_m.gguf");
    p
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[test]
#[ignore = "requires GGUF model download"]
fn model_loads_and_tokenizer_counts() {
    let path = model_path();
    assert!(path.exists(), "model not found at {path:?}");
    let llm = LocalLlm::open(&path, LocalLlmOptions::default()).expect("open model");
    assert!(!llm.model_id().is_empty());

    // The model tokenizer must count tokens exactly (much better than chars/4).
    let text = "Hello world, this is a tokenizer test.";
    let count = llm.count(text);
    let chars = text.chars().count();
    assert!(
        count > 0 && count < chars,
        "count {count} should be < chars {chars}"
    );

    // Truncation must produce a prefix, never the full text.
    let trunc = llm.truncate_to(text, 2);
    assert!(
        trunc.len() < text.len(),
        "truncated output should be shorter"
    );

    // Multi-byte text (CJK) must not panic.
    let ko = "김철수는 삼성전자에서 일한다.";
    let ko_count = llm.count(ko);
    assert!(ko_count > 0);
}

#[test]
#[ignore = "requires GGUF model download; slow (~15s)"]
fn model_generates_with_and_without_grammar() {
    let path = model_path();
    let llm = LocalLlm::open(&path, LocalLlmOptions::default()).expect("open model");

    // Unconstrained generation.
    let req = LlmRequest {
        model: llm.model_id().to_string(),
        system: Some("You are a helpful assistant. Reply with the single word: hello.".into()),
        prompt: "Say hello.".into(),
        json_schema: None,
        max_tokens: 32,
    };
    let resp = tokio_test_block_on(llm.complete(req));
    let resp = resp.expect("generation");
    assert!(!resp.text.is_empty(), "generation produced empty output");

    // Grammar-constrained generation — must start with { per the GBNF root.
    let grammar =
        oxibrain_core::extraction::grammar_from_registry(oxibrain_core::registry::core_v1());
    let req = LlmRequest {
        model: llm.model_id().to_string(),
        system: Some("You extract structured claims as JSON.".into()),
        prompt: "Alice works at Google.".into(),
        json_schema: None,
        max_tokens: 64,
    };
    let resp = tokio_test_block_on(llm.generate_constrained(req, &grammar));
    let resp = resp.expect("constrained generation");
    assert!(
        resp.text.trim_start().starts_with('{'),
        "grammar-constrained output should start with '{{', got: {}",
        resp.text.chars().take(30).collect::<String>()
    );
}

/// Minimal runtime helper to avoid a tokio-test dependency.
fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

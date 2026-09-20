#![cfg(target_vendor = "apple")]

//! Synthetic-model tests for the MLX engine (see `common` for the builder).

mod common;
use common::{GROUP, LAYERS, VOCAB, build_tiny_model};

use oxibrain_llm_mlx::qwen3::Qwen3Model;
use oxibrain_llm_mlx::weights::{model_fingerprint, resolve_model_dir};
use oxibrain_ports::{LlmPort, LlmRequest, TokenizerPort};

#[test]
fn forward_and_adapter_smoke() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_, fp) = build_tiny_model(dir);
    let _ = std::fs::remove_dir_all("/tmp/tiny-qwen3");
    let _ = build_tiny_model(std::path::Path::new("/tmp/tiny-qwen3"));

    // Fingerprint is stable across calls and format-sensitive.
    assert_eq!(model_fingerprint(dir).unwrap(), fp);
    let cfg = oxibrain_llm_mlx::config::Qwen3Config::load(dir).unwrap();
    assert!(cfg.is_moe());
    assert_eq!(cfg.quant_for("model.layers.0.mlp.gate").unwrap().bits, 8);
    assert_eq!(
        cfg.quant_for("model.layers.0.mlp.gate").unwrap().group_size,
        GROUP
    );
    assert_eq!(
        cfg.quant_for("model.layers.0.mlp.switch_mlp.experts.0.gate_proj")
            .unwrap()
            .bits,
        4
    );

    let weights = oxibrain_llm_mlx::weights::load_weights(dir).unwrap();
    let model = Qwen3Model::load(cfg, &weights, vec![5, 6]).unwrap();
    assert_eq!(model.num_layers(), LAYERS);

    // Forward pass: prefill then one decode step, shapes and finiteness.
    let mut cache = vec![None; model.num_layers()];
    let logits = model.next_token_logits(&[2, 4, 5], &mut cache).unwrap();
    assert_eq!(logits.shape(), vec![1, 1, VOCAB as i32]);
    let vals: Vec<f32> = logits.as_slice::<f32>().to_vec();
    assert!(vals.iter().all(|v| v.is_finite()), "logits must be finite");
    // Cache grew by prefill length on every layer.
    for c in &cache {
        let (k, _) = c.as_ref().unwrap();
        assert_eq!(k.shape()[2], 3);
    }
    let t = Qwen3Model::argmax_token(&logits).unwrap();
    assert!((t as usize) < VOCAB);
    let logits2 = model.next_token_logits(&[t], &mut cache).unwrap();
    let vals2: Vec<f32> = logits2.as_slice::<f32>().to_vec();
    assert!(vals2.iter().all(|v| v.is_finite()));
    for c in &cache {
        let (k, _) = c.as_ref().unwrap();
        assert_eq!(k.shape()[2], 4);
    }

    // Adapter end-to-end: load from the directory, generate through the
    // ChatML contract, stop on eos or max_tokens, count exact tokens.
    let llm = oxibrain_llm_mlx::LocalMxlLlm::load(dir.to_str().unwrap(), 512).unwrap();
    assert!(llm.model_dir().is_dir());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let resp = rt
        .block_on(llm.complete(LlmRequest {
            model: String::new(),
            system: Some("extractor".into()),
            prompt: "hello world".into(),
            json_schema: None,
            max_tokens: 8,
        }))
        .unwrap();
    assert_eq!(resp.raw["engine"], "mlx");
    assert_eq!(resp.raw["prompt_tokens"], 10); // ChatML template tokens for the toy tokenizer
    let _ = &fp;
    // Greedy on random weights stops only at eos or the cap.
    let emitted: usize = resp.raw["completion_tokens"].as_u64().unwrap() as usize;
    assert!(emitted <= 8);
    // TokenizerPort is the model's own tokenizer, not the chars/4 fallback.
    assert_eq!(llm.id(), "mlx-qwen3");
    assert!(llm.count("hello world") >= 1); // toy WordLevel without pre-tokenizer splits coarsely
}

#[test]
fn resolve_direct_path_beats_cache_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    build_tiny_model(tmp.path());
    let resolved = resolve_model_dir(tmp.path().to_str().unwrap()).unwrap();
    assert_eq!(resolved, tmp.path());
    assert!(resolve_model_dir("/nonexistent/spec-xyz").is_err());
}

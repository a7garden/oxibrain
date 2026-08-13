//! GBNF Spike — validate grammar-constrained extraction with llama-cpp-2.
//!
//! Usage:
//!   cargo run --release -- --model <path-to.gguf> [--max-tokens 512] [--seed 42]
//!
//! If --print-grammar is passed, prints the GBNF grammar and exits.

#![allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]

use anyhow::{bail, Context, Result};
use llama_cpp_2::json_schema_to_grammar;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::AddBos;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use oxibrain_core::extraction::{
    ExtractionResponse, build_extraction_prompt, grammar_from_registry, validate_claims,
};
use oxibrain_core::registry::core_v1;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Instant;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model_path: Option<PathBuf> = None;
    let mut max_tokens: i32 = 512;
    let mut seed: u32 = 42;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => model_path = args.next().map(PathBuf::from),
            "--max-tokens" | "-n" => {
                max_tokens = args.next().and_then(|s| s.parse().ok()).unwrap_or(512);
            }
            "--seed" | "-s" => {
                seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(42);
            }
            "--print-grammar" => {
                print!("{}", grammar_from_registry(core_v1()));
                return Ok(());
            }
            "--print-schema-grammar" => {
                let schema = oxibrain_core::extraction::schema_from_registry(core_v1());
                let g = json_schema_to_grammar(&schema.to_string())
                    .unwrap_or_else(|e| format!("ERROR: {e}"));
                print!("{g}");
                return Ok(());
            },
            _ => {}
        }
    }

    let predicates = core_v1();
    let system_prompt = build_extraction_prompt(predicates);
    let custom_grammar = grammar_from_registry(predicates);

    // Also generate the built-in schema→grammar for comparison.
    let schema = oxibrain_core::extraction::schema_from_registry(predicates);
    let schema_grammar = json_schema_to_grammar(&schema.to_string())
        .unwrap_or_else(|e| format!("# schema grammar error: {e}"));
    eprintln!("Custom grammar: {} bytes", custom_grammar.len());
    eprintln!("Schema grammar: {} bytes", schema_grammar.len());

    // Load episodes.
    let episodes_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("episodes");
    let mut episodes: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(&episodes_dir)
        .with_context(|| format!("reading {}", episodes_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let lang_dir = entry.path();
            for f in std::fs::read_dir(&lang_dir)? {
                let f = f?;
                if f.path().extension().is_some_and(|e| e == "txt") {
                    let name = format!(
                        "{}/{}",
                        lang_dir.file_name().unwrap().to_string_lossy(),
                        f.file_name().to_string_lossy()
                    );
                    episodes.push((name, std::fs::read_to_string(f.path())?));
                }
            }
        }
    }
    episodes.sort_by(|a, b| a.0.cmp(&b.0));
    eprintln!("Loaded {} episodes", episodes.len());

    let model_path = model_path.context("--model <path-to.gguf> required")?;

    // ─── Init llama.cpp ──────────────────────────────────────────────────
    let backend = LlamaBackend::init()?;
    let model_params = pin!(LlamaModelParams::default().with_n_gpu_layers(1000));
    eprintln!("Loading model: {}", model_path.display());
    let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)
        .context("failed to load model")?;
    eprintln!("Model loaded ({}B params).", model.n_params() / 1_000_000_000);

    let mut results: Vec<EpisodeResult> = Vec::new();

    // Try both grammars: report which one the parser accepts.
    let custom_works = test_grammar(&model, &custom_grammar);
    let schema_works = test_grammar(&model, &schema_grammar);
    eprintln!(
        "\nGrammar parse: custom={}, schema={}",
        if custom_works { "OK" } else { "FAIL" },
        if schema_works { "OK" } else { "FAIL" }
    );

    let grammar = if custom_works {
        eprintln!("Using custom grammar_from_registry()");
        custom_grammar.as_str()
    } else if schema_works {
        eprintln!("WARNING: custom grammar failed; falling back to json_schema_to_grammar()");
        schema_grammar.as_str()
    } else {
        bail!("both grammars failed to parse");
    };

    for (name, content) in &episodes {
        eprintln!("\n--- {name} ---");
        let result = run_episode(
            &backend,
            &model,
            &system_prompt,
            content,
            grammar,
            max_tokens,
            seed,
            name.clone(),
        );
        results.push(result);
    }

    // ─── Report ──────────────────────────────────────────────────────────
    let bar = "=".repeat(60);
    println!("\n{bar}");
    println!("GBNF Spike Results");
    println!("{bar}");
    println!(
        "Model: {} | max_tokens: {max_tokens} | seed: {seed}",
        model_path.display()
    );

    let total = results.len();
    let parse_ok = results.iter().filter(|r| r.parse_ok).count();
    let validate_ok = results.iter().filter(|r| r.validate_ok).count();
    let avg_ms = if total > 0 {
        results.iter().map(|r| r.wall_ms).sum::<u128>() / total as u128
    } else {
        0
    };

    println!("\n{:<30} {:>6} {:>6} {:>6}", "Episode", "Parse", "Valid", "ms");
    println!("{}", "-".repeat(52));
    for r in &results {
        println!(
            "{:<30} {:>6} {:>6} {:>6}",
            r.name,
            if r.parse_ok { "ok" } else { "FAIL" },
            if r.validate_ok { "ok" } else { "--" },
            r.wall_ms,
        );
        if let Some(e) = &r.error {
            println!("    -> {e}");
        }
    }

    println!();
    let pct = |n: usize| n as f64 * 100.0 / total.max(1) as f64;
    println!("Parse failures:       {}/{} ({:.0}%)", total - parse_ok, total, pct(total - parse_ok));
    println!("Validator rejections: {}/{} ({:.0}%)", total - validate_ok, total, pct(total - validate_ok));
    println!("Average wall time:    {avg_ms} ms/episode");

    if parse_ok == total {
        println!("\nDECISION: 0 parse failures -- llama-cpp-2 grammar wiring works.");
    } else {
        println!(
            "\nDECISION: {} parse failures -- grammar wiring has issues.",
            total - parse_ok
        );
    }

    Ok(())
}

/// Test whether llama.cpp's grammar parser accepts a grammar string.
fn test_grammar(model: &LlamaModel, grammar: &str) -> bool {
    LlamaSampler::grammar(model, grammar, "root").is_ok()
}

struct EpisodeResult {
    name: String,
    parse_ok: bool,
    validate_ok: bool,
    wall_ms: u128,
    error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn run_episode(
    backend: &LlamaBackend,
    model: &LlamaModel,
    system_prompt: &str,
    content: &str,
    grammar: &str,
    max_tokens: i32,
    seed: u32,
    name: String,
) -> EpisodeResult {
    let start = Instant::now();

    // Chat-formatted prompt (Qwen2.5 format; broadly compatible).
    let prompt = format!(
        "<|im_start|>system\n{system_prompt}<|im_end|>\n\
         <|im_start|>user\nExtract claims from this text:\n\n{content}<|im_end|>\n\
         <|im_start|>assistant\n"
    );

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(4096).expect("nonzero")))
        .with_n_threads(4);

    let mut ctx = match model.new_context(backend, ctx_params) {
        Ok(c) => c,
        Err(e) => {
            return EpisodeResult {
                name,
                parse_ok: false,
                validate_ok: false,
                wall_ms: start.elapsed().as_millis(),
                error: Some(format!("context creation: {e}")),
            }
        }
    };

    let tokens = match model.str_to_token(&prompt, AddBos::Never) {
        Ok(t) => t,
        Err(e) => {
            return EpisodeResult {
                name,
                parse_ok: false,
                validate_ok: false,
                wall_ms: start.elapsed().as_millis(),
                error: Some(format!("tokenization: {e}")),
            }
        }
    };

    let mut batch = LlamaBatch::new(2048, 1);
    let last_index = (tokens.len() - 1) as i32;
    for (i, token) in (0_i32..).zip(tokens.into_iter()) {
        let _ = batch.add(token, i, &[0], i == last_index);
    }
    if ctx.decode(&mut batch).is_err() {
        return EpisodeResult {
            name,
            parse_ok: false,
            validate_ok: false,
            wall_ms: start.elapsed().as_millis(),
            error: Some("prompt decode failed".into()),
        };
    }

    // Grammar-constrained sampler: grammar filters, greedy selects.
    let grammar_sampler = match LlamaSampler::grammar(model, grammar, "root") {
        Ok(s) => s,
        Err(e) => {
            return EpisodeResult {
                name,
                parse_ok: false,
                validate_ok: false,
                wall_ms: start.elapsed().as_millis(),
                error: Some(format!("grammar init: {e}")),
            }
        }
    };
    let _ = seed;
    let mut sampler = LlamaSampler::chain_simple([grammar_sampler, LlamaSampler::greedy()]);

    // Generation loop.
    // Generation loop. n_cur is the total position (prompt + output);
    // n_decode counts only output tokens and is bounded by max_tokens.
    let mut n_cur = batch.n_tokens();
    let mut n_decode = 0;
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut output = String::new();

    while n_decode < max_tokens {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);

        if model.is_eog_token(token) {
            break;
        }

        if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
            output.push_str(&piece);
        }

        batch.clear();
        let _ = batch.add(token, n_cur, &[0], true);
        n_cur += 1;
        n_decode += 1;

        if ctx.decode(&mut batch).is_err() {
            break;
        }
    }

    let wall_ms = start.elapsed().as_millis();
    eprintln!("  Generated {} chars in {wall_ms} ms", output.len());

    // Parse.
    let parsed: std::result::Result<ExtractionResponse, _> = serde_json::from_str(&output);
    let parse_ok = parsed.is_ok();
    let validate_ok = parsed.as_ref().is_ok_and(|resp| {
        let vr = validate_claims(&resp.claims, content, core_v1());
        vr.invalid.is_empty()
    });

    if !parse_ok {
        let tail_start = output.len().saturating_sub(100);
        eprintln!("  PARSE FAILED. Last 100 chars: ...{}", &output[tail_start..]);
    } else if let Ok(resp) = &parsed {
        if !validate_ok {
            eprintln!("  VALIDATION FAILED. {} claims", resp.claims.len());
        } else {
            eprintln!("  OK: {} claims extracted", resp.claims.len());
        }
    }

    EpisodeResult {
        name,
        parse_ok,
        validate_ok,
        wall_ms,
        error: None,
    }
}

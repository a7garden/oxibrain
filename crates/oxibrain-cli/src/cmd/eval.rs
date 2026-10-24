//! `oxibrain eval` — extraction evaluation suite (DESIGN §14.2).
//!
//! `fast` replays fixture responses through FakeLlmPort — no network, deterministic.
//! `full` requires a live provider (nightly only).

use oxibrain::Brain;
use oxibrain_core::eval::{
    ExtractedTriple, compute_metrics_with_fabrication, measure_fabrication_rate,
};
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_core::{SourceRef, TrustTier};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};

struct GoldenFixture {
    content: &'static str,
    canned_response: &'static str,
    expected_triples: Vec<ExtractedTriple>,
}

fn fixtures() -> Vec<GoldenFixture> {
    vec![
        GoldenFixture {
            content: "Alice works on ProjectX at Acme Corp",
            canned_response: r#"{"claims":[
                {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95},
                {"predicate":"employed_by","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"Acme Corp","entity_type":"Organization","span":[27,36]}},"polarity":"affirm","confidence":0.9}
            ]}"#,
            expected_triples: vec![
                triple("works_on", "Alice", "ProjectX"),
                triple("employed_by", "Alice", "Acme Corp"),
            ],
        },
        GoldenFixture {
            content: "Bob knows Carol. Bob was born in Seoul.",
            canned_response: r#"{"claims":[
                {"predicate":"knows","subject":{"surface":"Bob","entity_type":"Person","span":[0,3]},"object":{"kind":"entity","mention":{"surface":"Carol","entity_type":"Person","span":[10,15]}},"polarity":"affirm","confidence":0.9},
                {"predicate":"born_in","subject":{"surface":"Bob","entity_type":"Person","span":[17,20]},"object":{"kind":"entity","mention":{"surface":"Seoul","entity_type":"Place","span":[33,38]}},"polarity":"affirm","confidence":0.95}
            ]}"#,
            expected_triples: vec![
                triple("knows", "Bob", "Carol"),
                triple("born_in", "Bob", "Seoul"),
            ],
        },
        GoldenFixture {
            content: "Alice full name is Alice Smith.",
            canned_response: r#"{"claims":[
                {"predicate":"full_name","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"literal","literal_type":"text","value":"Alice Smith","span":[18,30]},"polarity":"affirm","confidence":0.95}
            ]}"#,
            expected_triples: vec![triple("full_name", "Alice", "Alice Smith")],
        },
    ]
}

fn triple(p: &str, s: &str, o: &str) -> ExtractedTriple {
    ExtractedTriple {
        predicate: p.into(),
        subject_surface: s.into(),
        object_surface: o.into(),
    }
}

fn test_extractor() -> ExtractorConfig {
    ExtractorConfig {
        model_id: "test-model".into(),
        prompt_version: 2, // v2: quote-based mentions (ADR-006); fixtures replay legacy spans via the compat ladder
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
        model_digest: None,
        provider_profile_id: None,
    }
}

pub async fn run(suite: &str) -> anyhow::Result<()> {
    match suite {
        "fast" => run_fast().await,
        "full" => {
            super::recall_eval::run(true).await?;
            run_extraction(true).await
        }
        "recall" => super::recall_eval::run(false).await,
        "reader" => super::recall_eval::run(true).await,
        "gate" => super::gate::run_with_dir(suite, None).await,
        "experiential" => super::gate::run_with_dir(suite, None).await,
        "experience" => super::gate::run_with_dir(suite, None).await,
        other => {
            anyhow::bail!(
                "unknown suite '{other}': use 'fast', 'full', 'recall', 'reader', 'gate', 'experience', or 'experiential'"
            )
        }
    }
}

async fn run_fast() -> anyhow::Result<()> {
    run_extraction(false).await
}

async fn run_extraction(live: bool) -> anyhow::Result<()> {
    let fixtures = fixtures();
    let provider = if live {
        Some(super::llm::from_env().await?)
    } else {
        None
    };
    let extractor = provider
        .as_ref()
        .map(|p| {
            super::llm::config(
                p.model_id.clone(),
                p.mechanism,
                p.model_digest.clone(),
                p.profile_id(),
            )
        })
        .unwrap_or_else(test_extractor);
    let replay = std::env::var_os("OXIBRAIN_EVAL_REPLAY")
        .map(std::fs::read)
        .transpose()?
        .map(|b| serde_json::from_slice::<serde_json::Value>(&b))
        .transpose()?;
    let mut captures = Vec::new();
    let mut all_extracted = Vec::new();
    let mut all_expected = Vec::new();
    let mut all_entity_surfaces: Vec<String> = Vec::new();

    for (ordinal, fixture) in fixtures.iter().enumerate() {
        let dir = tempfile::TempDir::new()?;
        let config = oxibrain::BrainConfig::at(dir.path());

        let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
        let llm = std::sync::Arc::new(FakeLlmPort::new());
        llm.respond_to(
            &fixture.content[..20.min(fixture.content.len())],
            LlmResponse {
                text: if let Some(replay) = &replay {
                    replay["fixtures"][ordinal]["raw_response"]
                        .as_str()
                        .ok_or_else(|| {
                            anyhow::anyhow!("replay is missing fixture {ordinal} raw_response")
                        })?
                        .to_string()
                } else {
                    fixture.canned_response.into()
                },
                raw: serde_json::Value::Null,
            },
        );

        let port: std::sync::Arc<dyn oxibrain_ports::LlmPort> =
            provider.as_ref().map(|p| p.port.clone()).unwrap_or(llm);
        let brain = Brain::with_llm(config.clone(), clock, port).await?;
        let space = brain.ensure_space("eval").await?;

        let ep_id = brain
            .ingest(
                &space,
                fixture.content.into(),
                SourceRef::Note {
                    path: "fixture.md".into(),
                },
                TrustTier::Trusted,
                &extractor.id(),
            )
            .await?;

        let summary = brain.extract_one(&space, &ep_id, &extractor).await?;

        if !live && replay.is_none() && summary.extracted == 0 {
            anyhow::bail!("fixture extracted 0 claims: {}", fixture.content);
        }

        let extracted = brain.debug_triples(&space).await?;
        // Collect entity surfaces for fabrication measurement (§17.3, 10.7).
        for t in &extracted {
            all_entity_surfaces.push(t.subject_surface.clone());
            all_entity_surfaces.push(t.object_surface.clone());
        }
        if live {
            let conn = oxibrain_store::Store::open_read_only(&config.dir)?;
            let raw:String=conn.query_row("SELECT raw_response FROM extractions WHERE episode_id=?1 UNION ALL SELECT raw_response FROM extraction_failures WHERE episode_id=?1 LIMIT 1",[&ep_id],|r|r.get(0))?;
            captures.push(serde_json::json!({"source":fixture.content,"raw_response":raw,"expected":fixture.expected_triples,"extracted":extracted}));
        }
        all_extracted.extend(extracted);
        all_expected.extend(fixture.expected_triples.clone());
    }

    // Fabricated entity rate measured from source text (§17.3, 10.7).
    // Each entity surface must appear verbatim in some fixture's content; the
    // validator enforces this and measure_fabrication_rate proves it. No
    // hardcoded 0.0.
    let combined_source: String = fixtures
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join(" ");
    let global_rate = measure_fabrication_rate(&all_entity_surfaces, &combined_source);

    let metrics = compute_metrics_with_fabrication(&all_extracted, &all_expected, global_rate);
    if live {
        let config: serde_json::Value = serde_json::from_slice(&std::fs::read(
            std::env::var_os("OXIBRAIN_EVAL_CONFIG")
                .ok_or_else(|| anyhow::anyhow!("missing evaluation config"))?,
        )?)?;
        let path = std::path::PathBuf::from(
            config["output"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("evaluation output must be a path"))?,
        )
        .with_extension("extraction.json");
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &serde_json::json!({"schema":"extraction-eval/1","extractor":extractor,"metrics":metrics,"fixtures":captures}),
            )?,
        )?;
    }
    println!();
    println!(
        "Fabricated entity rate: {:.3}  (gate: 0.000)",
        metrics.fabricated_entity_rate
    );
    println!(
        "Statement precision:    {:.3}  (gate: ≥ 0.90)",
        metrics.statement_precision
    );
    println!(
        "Statement recall:       {:.3}  (gate: ≥ 0.70)",
        metrics.statement_recall
    );
    println!();
    println!("Extracted: {all_extracted:?}");
    println!("Expected:  {all_expected:?}");

    match metrics.check_gates() {
        Ok(()) => {
            println!();
            println!("✅ All §14.2 quality gates passed.");
            Ok(())
        }
        Err(e) => {
            println!();
            println!("❌ Quality gates failed: {e}");
            anyhow::bail!("quality gates failed: {e}");
        }
    }
}

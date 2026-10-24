//! Ignored, release-only integration runner for the pinned LongMemEval-S subset.
//!
//! This adapter stores each source session verbatim (with role/date boundary
//! markers) as an untrusted session episode. It does not run extraction or a
//! reader. Gold is opened only after every retrieval request has completed.

use anyhow::{Context, ensure};
use oxibrain::{Brain, BrainConfig, ReadPolicy, RecallRequest, Timestamp};
use oxibrain_core::context::render_context;
use oxibrain_core::evidence::{EvidenceCitation, SourceCoordinate};
use oxibrain_core::read_channels::ChannelReceipt;
use oxibrain_core::retrieval::SearchPlane;
use oxibrain_ports::{EmbeddingPort, TokenizerPort};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const SCHEMA: &str = "oxibrain-external-memory-qa/1";
const DATASET_REVISION: &str = "98d7416c24c778c2fee6e6f3006e7a073259d48f";
const UPSTREAM_DATA_SHA256: &str =
    "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442";
const LICENSE_SHA256: &str = "d3c4b9aa54759df6ded337978a6f3b55b75615e5e4525c3b82d7e2627d4b9732";
const BUDGETS: [usize; 2] = [2_000, 3_000];

#[derive(Debug, Clone, Copy)]
struct DatasetProfile {
    name: &'static str,
    input_sha256: &'static str,
    gold_sha256: &'static str,
    manifest_sha256: &'static str,
    freezer_sha256: &'static str,
    cases: usize,
    full_history_sessions: usize,
    manifest_input_path: &'static str,
    manifest_gold_path: &'static str,
}

const V1_PROFILE: DatasetProfile = DatasetProfile {
    name: "v1",
    input_sha256: "3d098e4e4aa0f0c2a9a6f279921b23d25ab7c81d94432e57975a69a0d06ca",
    gold_sha256: "a2e0f50dd2cc530454b24155d8da0701d54d4c15880400d1147f1268c1dfcf49",
    manifest_sha256: "f489c4b5a3b7c4088cfbccbe8cb11181ec1a485babe0c115d5a46cc2e35b670d",
    freezer_sha256: "9c88e7b4de701ec2e9fbb05d04289311edf0df788de018ac8b852a5660a92cac",
    cases: 20,
    full_history_sessions: 943,
    manifest_input_path: "input.jsonl",
    manifest_gold_path: "gold.jsonl",
};

const V2_PROFILE: DatasetProfile = DatasetProfile {
    name: "v2",
    input_sha256: "2ed7811b885afaaf04752248c8e3a9a7a4823f6eefb971ab67ab37cc085345a3",
    gold_sha256: "d4fac57d5b98ab672e05e5ebfbf789e110e4cc480ce031135463fb7160c124d3",
    manifest_sha256: "b0b3b18296fe78f3f1774d32c4a2c240aa63f642a54e76582e1d1c6d9bc5285f",
    freezer_sha256: "fdc7d4351fbf09e73564c14c6804a5564746711f4ac259f629cefc93a7d6fd0c",
    cases: 20,
    full_history_sessions: 980,
    manifest_input_path: "/tmp/oxibrain-longmemeval-s-public-v2/input.jsonl",
    manifest_gold_path: "/tmp/oxibrain-longmemeval-s-public-v2/gold.jsonl",
};

const V3_PROFILE: DatasetProfile = DatasetProfile {
    name: "v3",
    input_sha256: "66b0f492c3315a31dba5e6c78fe06311e09ed76f9e981406fdbfb10b6cfde06d",
    gold_sha256: "65bc6612cd104cc7a05dbf5d02c2a773c16e53f0d71940ad6efbdc73f910b4fd",
    manifest_sha256: "56c009f6e0ace91b2297bc9b642d75602347fcee70314330662472d79fef7322",
    freezer_sha256: "37b06022846cce812680e9529994929d80b717fdfd32fd3afa841f29d5462f4f",
    cases: 20,
    full_history_sessions: 931,
    manifest_input_path: "/tmp/oxibrain-longmemeval-s-public-v3/input.jsonl",
    manifest_gold_path: "/tmp/oxibrain-longmemeval-s-public-v3/gold.jsonl",
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputCase {
    case_id: String,
    question: String,
    question_date: String,
    history_sessions: Vec<InputSession>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputSession {
    session_id: String,
    date: String,
    turns: Vec<InputTurn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputTurn {
    content: String,
    role: String,
}

#[derive(Debug, Deserialize)]
struct GoldCase {
    case_id: String,
    question_type: String,
    abstention: bool,
    answer: Value,
    #[serde(default)]
    answer_session_ids: Vec<String>,
    #[serde(default)]
    annotated_answer_turns: Vec<AnnotatedTurn>,
}

#[derive(Debug, Deserialize)]
struct AnnotatedTurn {
    session_id: String,
    session_index: usize,
    turn_index: usize,
}

#[derive(Debug)]
struct SessionSource {
    upstream_session_id: String,
    session_index: usize,
    reference: String,
    revision: String,
    episode_id: String,
    body: String,
    turn_ranges: Vec<TurnRange>,
}

#[derive(Debug, Clone)]
struct TurnRange {
    turn_index: usize,
    role: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Serialize)]
struct ReleaseReport {
    schema: &'static str,
    status: &'static str,
    dataset: DatasetReport,
    run: RunConfiguration,
    aggregate: BTreeMap<usize, BudgetAggregate>,
    cases: Vec<CaseReport>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct DatasetReport {
    dataset: &'static str,
    profile: &'static str,
    upstream_revision: &'static str,
    upstream_file_sha256: &'static str,
    manifest_sha256: String,
    input_sha256: String,
    gold_sha256: String,
    license: &'static str,
    license_sha256: String,
    cases: usize,
    full_history_sessions: usize,
    source_rows_scanned: usize,
    annotation_caveat: String,
    retrieval_caveat: String,
}

#[derive(Debug, Serialize)]
struct RunConfiguration {
    render_mode: String,
    read_policy: Value,
    budgets: [usize; 2],
    tokenizer: ArtifactIdentity,
    embedder: ArtifactIdentity,
    model_load_ms: f64,
    source_time_interpretation: &'static str,
    reader: &'static str,
    extraction: &'static str,
    monetary_cost: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ArtifactIdentity {
    path: String,
    id: String,
    blake3: String,
    model_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    case_id: String,
    question_type: String,
    annotated_abstention: bool,
    answer_session_annotation_count: usize,
    mapped_answer_session_occurrence_count: usize,
    answer_turn_annotation_count: usize,
    history_sessions: usize,
    history_turns: usize,
    question_bytes: usize,
    source_turn_bytes: usize,
    indexed_episode_bytes: usize,
    recorded_sessions: usize,
    indexed_sources: usize,
    embedded_windows: usize,
    recording_ms: f64,
    indexing_ms: f64,
    embedding_ms: f64,
    queries: Vec<QueryReport>,
}

#[derive(Debug, Serialize)]
struct QueryReport {
    budget: usize,
    read_ms: f64,
    context_bytes: usize,
    context_tokens: usize,
    tokens_receipt: Option<Value>,
    scope_receipt: Value,
    channel_receipts: Vec<ChannelReceipt>,
    index_coverage_receipt: Value,
    execution_receipt: Value,
    freshness_receipt: Value,
    truth_selection_receipt: Option<Value>,
    selection_receipt: Value,
    selected_evidence: Vec<EvidenceCitation>,
    full_history_session_retrieval: FullHistorySessionRetrieval,
    answer_turn_byte_coverage: Option<AnswerTurnCoverage>,
    answer_session_retrieval: Option<AnswerSessionRetrieval>,
}

#[derive(Debug, Serialize)]
struct FullHistorySessionRetrieval {
    total_history_sessions: usize,
    selected_history_sessions: usize,
    selected_fraction: f64,
    selected_session_ids: Vec<String>,
    selected_session_occurrences: Vec<SessionOccurrence>,
    unresolved_session_references: Vec<String>,
    interpretation: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct SessionOccurrence {
    session_id: String,
    session_index: usize,
}

#[derive(Debug, Serialize)]
struct AnswerTurnCoverage {
    status: &'static str,
    annotated_turns: usize,
    total_annotated_bytes: usize,
    retrieved_annotated_bytes: usize,
    byte_fraction: Option<f64>,
    turns_with_any_retrieved_bytes: usize,
    turns_fully_retrieved: usize,
    per_turn: Vec<TurnCoverage>,
    interpretation: &'static str,
}

#[derive(Debug, Serialize)]
struct TurnCoverage {
    session_id: String,
    session_index: usize,
    turn_index: usize,
    role: String,
    source: SourceCoordinate,
    turn_bytes: usize,
    retrieved_bytes: usize,
    fraction: Option<f64>,
}

#[derive(Debug, Serialize)]
struct AnswerSessionRetrieval {
    status: &'static str,
    gold_answer_session_ids: Vec<String>,
    missing_gold_session_ids: Vec<String>,
    retrieved_gold_session_ids: Vec<String>,
    gold_answer_session_occurrences: Vec<SessionOccurrence>,
    retrieved_gold_session_occurrences: Vec<SessionOccurrence>,
    any_retrieved: Option<bool>,
    all_retrieved: Option<bool>,
}

#[derive(Debug)]
struct CapturedCase {
    case: CaseReport,
    sources: Vec<SessionSource>,
}

#[derive(Debug, Serialize)]
struct BudgetAggregate {
    answerable_cases: usize,
    abstention_cases: usize,
    answer_turn_annotated_bytes: usize,
    answer_turn_retrieved_bytes: usize,
    weighted_answer_turn_byte_coverage: Option<f64>,
    answer_session_measured_cases: usize,
    any_answer_session_retrieved_cases: usize,
    all_answer_sessions_retrieved_cases: usize,
    mean_full_history_session_selection_fraction: Option<f64>,
    abstention_scoring: &'static str,
}

#[tokio::test]
#[ignore = "release-only external benchmark; requires explicit dataset/model/tokenizer/output env vars"]
async fn longmemeval_release_runner() {
    run_release_benchmark()
        .await
        .expect("LongMemEval release run failed");
}

async fn run_release_benchmark() -> anyhow::Result<()> {
    ensure!(
        std::env::var("OXIBRAIN_RUN_LONGMEMEVAL").ok().as_deref() == Some("1"),
        "set OXIBRAIN_RUN_LONGMEMEVAL=1 to opt into the external release run"
    );
    let profile_choice = std::env::var_os("OXIBRAIN_LONGMEMEVAL_SUBSET")
        .map(|value| value.into_string())
        .transpose()
        .map_err(|_| anyhow::anyhow!("OXIBRAIN_LONGMEMEVAL_SUBSET must be valid UTF-8"))?;
    let profile = dataset_profile(profile_choice.as_deref())?;
    let dataset_dir = required_path("OXIBRAIN_LONGMEMEVAL_DIR")?;
    let tokenizer_path = required_path("OXIBRAIN_TOKENIZER_JSON")?;
    let embed_model_path = required_path("OXIBRAIN_EMBED_MODEL")?;
    let output_path = required_path("OXIBRAIN_LONGMEMEVAL_OUTPUT")?;
    ensure!(
        output_path.is_absolute(),
        "OXIBRAIN_LONGMEMEVAL_OUTPUT must be an absolute path"
    );
    ensure!(
        !output_path.exists(),
        "output already exists; choose a fresh OXIBRAIN_LONGMEMEVAL_OUTPUT path"
    );

    let manifest_bytes = std::fs::read(dataset_dir.join("manifest.json"))
        .context("read external dataset manifest")?;
    ensure_eq_digest(&manifest_bytes, profile.manifest_sha256, "manifest.json")?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest, profile)?;
    let input_bytes =
        std::fs::read(dataset_dir.join("input.jsonl")).context("read external input corpus")?;
    ensure_eq_digest(&input_bytes, profile.input_sha256, "input.jsonl")?;
    let freezer_bytes = std::fs::read(dataset_dir.join("freeze_subset.py"))
        .context("read external subset freezer")?;
    ensure_eq_digest(&freezer_bytes, profile.freezer_sha256, "freeze_subset.py")?;
    let license_bytes =
        std::fs::read(dataset_dir.join("LICENSE.txt")).context("read upstream license")?;
    ensure_eq_digest(&license_bytes, LICENSE_SHA256, "LICENSE.txt")?;

    let input_cases = parse_jsonl::<InputCase>(&input_bytes, "input.jsonl")?;
    ensure!(
        input_cases.len() == profile.cases,
        "unexpected input case count"
    );
    let session_count: usize = input_cases
        .iter()
        .map(|case| case.history_sessions.len())
        .sum();
    ensure!(
        session_count == profile.full_history_sessions,
        "unexpected full-history session count"
    );
    validate_input_cases(&input_cases)?;

    let render_mode =
        std::env::var("OXIBRAIN_LONGMEMEVAL_RENDER_MODE").unwrap_or_else(|_| "default".to_string());
    let policy = read_policy_for_render_mode(&render_mode)?;

    let tokenizer = Arc::new(oxibrain_tokenizer_local::FileTokenizer::open(
        &tokenizer_path,
    )?);
    ensure!(
        tokenizer.quality() == oxibrain_ports::tokenizer::TokenizerQuality::Exact,
        "release evaluation requires an exact FileTokenizer artifact"
    );
    let tokenizer_digest = tokenizer
        .artifact_digest()
        .context("tokenizer artifact has no digest")?
        .to_string();
    check_optional_pin(
        "OXIBRAIN_LONGMEMEVAL_EXPECT_TOKENIZER_BLAKE3",
        &tokenizer_digest,
    )?;
    let tokenizer_identity = ArtifactIdentity {
        path: tokenizer_path.display().to_string(),
        id: tokenizer.id().to_string(),
        blake3: tokenizer_digest,
        model_name: None,
    };
    let tokenizer_port: Arc<dyn TokenizerPort> = tokenizer.clone();

    let model_load_started = Instant::now();
    let embedder = Arc::new(oxibrain_embed_local::LocalEmbedder::open(
        &embed_model_path,
        Default::default(),
    )?);
    let model_load_ms = elapsed_ms(model_load_started);
    let embedder_digest = file_blake3(&embed_model_path)?;
    check_optional_pin("OXIBRAIN_LONGMEMEVAL_EXPECT_EMBED_BLAKE3", &embedder_digest)?;
    let embedder_identity = embedder
        .identity()
        .context("local embedder has no stable identity")?;
    let embedder_model_name = embedder.model_id().to_string();
    ensure!(
        embedder_identity.contains(&embedder_digest),
        "embedder identity does not contain the loaded artifact digest"
    );
    let embedder_report = ArtifactIdentity {
        path: embed_model_path.display().to_string(),
        id: embedder_identity,
        blake3: embedder_digest,
        model_name: Some(embedder_model_name),
    };
    let embedder_port: Arc<dyn EmbeddingPort> = embedder;

    // Input-only phase: no gold file or answer labels are read until all queries finish.
    let mut captured = Vec::with_capacity(input_cases.len());
    for (case_index, input) in input_cases.iter().enumerate() {
        eprintln!(
            "LongMemEval retrieval: case {}/{}",
            case_index + 1,
            input_cases.len()
        );
        captured.push(
            run_case(
                input,
                tokenizer_port.clone(),
                embedder_port.clone(),
                &policy,
            )
            .await?,
        );
    }

    // Gold is evaluator-only and is loaded after retrieval, never placed in a prompt or store.
    let gold_bytes =
        std::fs::read(dataset_dir.join("gold.jsonl")).context("read evaluator-only gold file")?;
    ensure_eq_digest(&gold_bytes, profile.gold_sha256, "gold.jsonl")?;
    let gold_cases = parse_jsonl::<GoldCase>(&gold_bytes, "gold.jsonl")?;
    ensure!(
        gold_cases.len() == profile.cases,
        "unexpected gold case count"
    );
    let gold_by_id: BTreeMap<_, _> = gold_cases
        .iter()
        .map(|gold| (gold.case_id.clone(), gold))
        .collect();
    ensure!(
        gold_by_id.len() == gold_cases.len(),
        "duplicate case id in gold file"
    );
    ensure!(
        gold_by_id.len() == captured.len(),
        "input/gold case count mismatch"
    );
    for capture in &mut captured {
        let gold = gold_by_id
            .get(&capture.case.case_id)
            .context("gold row missing for input case")?;
        attach_gold_metrics(capture, gold)?;
    }

    let cases: Vec<_> = captured.into_iter().map(|capture| capture.case).collect();
    let aggregate = aggregate_by_budget(&cases);
    let report = ReleaseReport {
        schema: "oxibrain-longmemeval-recall/1",
        status: "complete",
        dataset: DatasetReport {
            dataset: "LongMemEval cleaned LongMemEval_S",
            profile: profile.name,
            upstream_revision: DATASET_REVISION,
            upstream_file_sha256: UPSTREAM_DATA_SHA256,
            manifest_sha256: sha256_hex(&manifest_bytes),
            input_sha256: sha256_hex(&input_bytes),
            gold_sha256: sha256_hex(&gold_bytes),
            license: "MIT",
            license_sha256: sha256_hex(&license_bytes),
            cases: profile.cases,
            full_history_sessions: profile.full_history_sessions,
            source_rows_scanned: manifest["source_rows_scanned"]
                .as_u64()
                .context("manifest source_rows_scanned missing")?
                as usize,
            annotation_caveat: manifest["annotation_caveat"]
                .as_str()
                .context("manifest annotation caveat missing")?
                .to_string(),
            retrieval_caveat: manifest["retrieval_caveat"]
                .as_str()
                .context("manifest retrieval caveat missing")?
                .to_string(),
        },
        run: RunConfiguration {
            render_mode,
            read_policy: serde_json::to_value(policy)?,
            budgets: BUDGETS,
            tokenizer: tokenizer_identity,
            embedder: embedder_report,
            model_load_ms,
            source_time_interpretation: "The source wall-clock values are interpreted as UTC for stable ordering; original date strings remain in each stored session body.",
            reader: "not run; no answer correctness or abstention behavior is scored",
            extraction: "not run; full history is stored as untrusted session text",
            monetary_cost: None,
        },
        aggregate,
        cases,
        limitations: vec![
            "This is a 20-case selected subset, not the complete 500-row source corpus.",
            "The benchmark is English-only, uses curated questions, and is not real-user dialogue.",
            "Answer-turn annotations cover entire source turns, not minimal supporting substrings.",
            "Repeated upstream session IDs are indexed as separate input-ordinal occurrences; answer-session mapping uses annotated session indices and fails when a duplicate ID is ambiguous.",
            "Full-history session selection fraction describes retrieval breadth; it is not a relevance score.",
            "Abstention annotations are not counted as successful abstentions because no reader was run.",
            "No string-only answer matching or answer correctness score is computed.",
        ],
    };

    let output_parent = output_path
        .parent()
        .context("output path has no parent directory")?;
    std::fs::create_dir_all(output_parent)?;
    let mut output_bytes = serde_json::to_vec_pretty(&report)?;
    output_bytes.push(b'\n');
    let temporary = tempfile::NamedTempFile::new_in(output_parent)?;
    std::fs::write(temporary.path(), &output_bytes)?;
    temporary
        .persist(&output_path)
        .with_context(|| format!("persist report to {}", output_path.display()))?;
    eprintln!("LongMemEval report: {}", output_path.display());
    Ok(())
}

async fn run_case(
    input: &InputCase,
    tokenizer: Arc<dyn TokenizerPort>,
    embedder: Arc<dyn EmbeddingPort>,
    policy: &ReadPolicy,
) -> anyhow::Result<CapturedCase> {
    let temporary_store = tempfile::tempdir()?;
    let brain = Brain::open(BrainConfig::at(temporary_store.path()))
        .await?
        .with_tokenizer(tokenizer)
        .with_embedder(embedder)
        .with_read_policy(policy.clone())?;
    let space = brain.ensure_space("longmemeval").await?;

    let mut sources = Vec::with_capacity(input.history_sessions.len());
    let indexed_session_ids = indexed_session_ids(&input.history_sessions);
    let mut source_turn_bytes = 0;
    let mut indexed_episode_bytes = 0;
    let mut history_turns = 0;
    let recording_started = Instant::now();
    for (session_index, session) in input.history_sessions.iter().enumerate() {
        ensure!(
            !session.session_id.is_empty() && !session.turns.is_empty(),
            "empty source session in case {}",
            input.case_id
        );
        let occurred_at = timestamp_from_source_date(&session.date)?;
        let (body, turn_ranges) = format_session(session)?;
        let revision = oxibrain_core::id::content_hash(&body).hex().to_uppercase();
        source_turn_bytes += session
            .turns
            .iter()
            .map(|turn| turn.content.len())
            .sum::<usize>();
        history_turns += session.turns.len();
        indexed_episode_bytes += body.len();
        let indexed_session_id = &indexed_session_ids[session_index];
        let episode_id = brain
            .record_session(&space, indexed_session_id, body.clone(), occurred_at)
            .await?;
        sources.push(SessionSource {
            upstream_session_id: session.session_id.clone(),
            session_index,
            reference: format!("session://{indexed_session_id}"),
            revision,
            episode_id,
            body,
            turn_ranges,
        });
    }
    let recording_ms = elapsed_ms(recording_started);

    let indexing_started = Instant::now();
    let indexed_sources = brain.index_memory().await?;
    let indexing_ms = elapsed_ms(indexing_started);
    let embedding_started = Instant::now();
    let embedded_windows = brain.embed_pending_memory(usize::MAX).await?;
    let embedding_ms = elapsed_ms(embedding_started);

    let mut queries = Vec::with_capacity(BUDGETS.len());
    for budget in BUDGETS {
        let mut request = RecallRequest::new(&space, &input.question, budget);
        request.require_exact_tokens = true;
        request.planes = [SearchPlane::Memory].into_iter().collect();
        let read_started = Instant::now();
        let response = brain.recall_with_request(request).await?;
        let read_ms = elapsed_ms(read_started);
        let context = render_context(&response.data.context);
        let context_bytes = context.len();
        let tokens = response.data.context.total_tokens;
        ensure!(tokens <= budget, "retrieval exceeded exact token budget");
        ensure!(
            response.meta.index_coverage["complete"] == true,
            "raw source index is incomplete for case {}",
            input.case_id
        );
        let tokens_receipt = response
            .meta
            .tokens
            .as_ref()
            .context("exact token receipt missing")?;
        ensure!(
            tokens_receipt["exact"].as_bool() == Some(true)
                && tokens_receipt["spent"].as_u64() == Some(tokens as u64)
                && tokens_receipt["budget"].as_u64() == Some(budget as u64),
            "exact tokenizer receipt missing for case {}",
            input.case_id
        );
        let dense_receipt = response
            .meta
            .channels
            .iter()
            .find(|receipt| receipt.plane == "memory_sources" && receipt.channel == "dense")
            .context("dense source channel receipt missing")?;
        ensure!(
            dense_receipt.eligible
                && dense_receipt.executed
                && dense_receipt.fallback.is_none()
                && dense_receipt.indexed.is_some()
                && dense_receipt.total.is_some()
                && dense_receipt.indexed == dense_receipt.total,
            "dense source vectors are not fully indexed for case {}",
            input.case_id
        );

        let selected_evidence = response.data.evidence;
        validate_selected_coordinates(&selected_evidence, &sources)?;
        let full_history_session_retrieval = full_history_retrieval(&selected_evidence, &sources);
        let selection_receipt = serde_json::json!({
            "layers_kept": response.meta.layers_kept,
            "dropped": response.meta.dropped,
            "metadata_decisions": response.meta.decisions,
            "unit_decisions": response.data.decisions,
            "context_truncated": response.data.context.truncated,
        });
        queries.push(QueryReport {
            budget,
            read_ms,
            context_bytes,
            context_tokens: tokens,
            tokens_receipt: response.meta.tokens,
            scope_receipt: response.meta.scope,
            channel_receipts: response.meta.channels,
            index_coverage_receipt: response.meta.index_coverage,
            execution_receipt: response.meta.execution,
            freshness_receipt: serde_json::to_value(response.meta.freshness)?,
            truth_selection_receipt: response
                .meta
                .truth_selection
                .map(serde_json::to_value)
                .transpose()?,
            selection_receipt,
            selected_evidence,
            full_history_session_retrieval,
            answer_turn_byte_coverage: None,
            answer_session_retrieval: None,
        });
    }

    let case = CaseReport {
        case_id: input.case_id.clone(),
        question_type: String::new(),
        annotated_abstention: false,
        answer_session_annotation_count: 0,
        mapped_answer_session_occurrence_count: 0,
        answer_turn_annotation_count: 0,
        history_sessions: input.history_sessions.len(),
        history_turns,
        question_bytes: input.question.len(),
        source_turn_bytes,
        indexed_episode_bytes,
        recorded_sessions: sources.len(),
        indexed_sources,
        embedded_windows,
        recording_ms,
        indexing_ms,
        embedding_ms,
        queries,
    };
    Ok(CapturedCase { case, sources })
}

fn attach_gold_metrics(capture: &mut CapturedCase, gold: &GoldCase) -> anyhow::Result<()> {
    // Gold `answer` is parsed only to validate the pinned schema; it is never emitted or scored.
    let _gold_answer_value = &gold.answer;
    ensure!(
        gold.case_id == capture.case.case_id,
        "gold/input case id mismatch"
    );
    capture.case.question_type = gold.question_type.clone();
    capture.case.annotated_abstention = gold.abstention;
    capture.case.answer_session_annotation_count = gold.answer_session_ids.len();
    capture.case.mapped_answer_session_occurrence_count = gold_answer_occurrences(
        &gold.answer_session_ids,
        &gold.annotated_answer_turns,
        &capture.sources,
    )?
    .len();
    capture.case.answer_turn_annotation_count = gold.annotated_answer_turns.len();
    for query in &mut capture.case.queries {
        query.answer_turn_byte_coverage = Some(answer_turn_coverage(
            &gold.annotated_answer_turns,
            &capture.sources,
            &query.selected_evidence,
        )?);
        query.answer_session_retrieval = Some(answer_session_retrieval(
            gold,
            &capture.sources,
            &query.full_history_session_retrieval,
        )?);
    }
    Ok(())
}

fn answer_turn_coverage(
    annotations: &[AnnotatedTurn],
    sources: &[SessionSource],
    evidence: &[EvidenceCitation],
) -> anyhow::Result<AnswerTurnCoverage> {
    if annotations.is_empty() {
        return Ok(AnswerTurnCoverage {
            status: "no_annotated_answer_turns",
            annotated_turns: 0,
            total_annotated_bytes: 0,
            retrieved_annotated_bytes: 0,
            byte_fraction: None,
            turns_with_any_retrieved_bytes: 0,
            turns_fully_retrieved: 0,
            per_turn: Vec::new(),
            interpretation: "No per-turn byte range was labeled; this is not evidence of successful abstention.",
        });
    }

    let mut total_bytes = 0usize;
    let mut retrieved_bytes = 0usize;
    let mut turns_with_any = 0usize;
    let mut turns_fully = 0usize;
    let mut per_turn = Vec::with_capacity(annotations.len());
    let mut seen_turns = BTreeSet::new();
    for annotation in annotations {
        ensure!(
            seen_turns.insert((annotation.session_index, annotation.turn_index)),
            "duplicate annotated answer turn"
        );
        let source = sources
            .get(annotation.session_index)
            .context("annotated answer session missing")?;
        ensure!(
            source.upstream_session_id == annotation.session_id,
            "annotated answer session ID does not match input index"
        );
        let turn = source
            .turn_ranges
            .iter()
            .find(|turn| turn.turn_index == annotation.turn_index)
            .context("annotated answer turn missing")?;
        let source_coordinate = SourceCoordinate {
            reference: source.reference.clone(),
            revision: source.revision.clone(),
            view: "episode-utf8/1".into(),
            view_digest: Some(source.revision.clone()),
            decoder: None,
            start: turn.start,
            end: turn.end,
            document_uuid: None,
        };
        let turn_bytes = turn.end.saturating_sub(turn.start);
        let retrieved = selected_overlap_bytes(&source.reference, turn.start, turn.end, evidence);
        total_bytes += turn_bytes;
        retrieved_bytes += retrieved;
        turns_with_any += usize::from(retrieved > 0);
        turns_fully += usize::from(retrieved == turn_bytes && turn_bytes > 0);
        per_turn.push(TurnCoverage {
            session_id: source.upstream_session_id.clone(),
            session_index: annotation.session_index,
            turn_index: turn.turn_index,
            role: turn.role.clone(),
            source: source_coordinate,
            turn_bytes,
            retrieved_bytes: retrieved,
            fraction: (turn_bytes > 0).then_some(retrieved as f64 / turn_bytes as f64),
        });
    }
    Ok(AnswerTurnCoverage {
        status: if total_bytes == 0 {
            "zero_annotated_bytes"
        } else {
            "measured"
        },
        annotated_turns: annotations.len(),
        total_annotated_bytes: total_bytes,
        retrieved_annotated_bytes: retrieved_bytes,
        byte_fraction: (total_bytes > 0).then_some(retrieved_bytes as f64 / total_bytes as f64),
        turns_with_any_retrieved_bytes: turns_with_any,
        turns_fully_retrieved: turns_fully,
        per_turn,
        interpretation: "Coverage is the union of selected source-coordinate bytes intersecting each entire annotated answer turn; annotations are not minimal answer spans.",
    })
}

fn answer_session_retrieval(
    gold: &GoldCase,
    sources: &[SessionSource],
    full_history: &FullHistorySessionRetrieval,
) -> anyhow::Result<AnswerSessionRetrieval> {
    let occurrences = gold_answer_occurrences(
        &gold.answer_session_ids,
        &gold.annotated_answer_turns,
        sources,
    )?;
    let selected: BTreeSet<_> = full_history
        .selected_session_occurrences
        .iter()
        .map(|occurrence| occurrence.session_index)
        .collect();
    let missing: Vec<_> = gold
        .answer_session_ids
        .iter()
        .filter(|id| {
            !sources
                .iter()
                .any(|source| source.upstream_session_id.as_str() == id.as_str())
        })
        .cloned()
        .collect();
    let status = if gold.abstention {
        "abstention_not_scored_without_reader"
    } else if gold.answer_session_ids.is_empty() {
        "unavailable_no_answer_session_annotations"
    } else if !missing.is_empty() {
        "unavailable_unmapped_gold_session_ids"
    } else {
        "measured"
    };
    let retrieved: Vec<_> = occurrences
        .iter()
        .filter(|occurrence| selected.contains(&occurrence.session_index))
        .cloned()
        .collect();
    let can_score = status == "measured";
    Ok(AnswerSessionRetrieval {
        status,
        gold_answer_session_ids: gold.answer_session_ids.clone(),
        missing_gold_session_ids: missing,
        retrieved_gold_session_ids: retrieved
            .iter()
            .map(|occurrence| occurrence.session_id.clone())
            .collect(),
        gold_answer_session_occurrences: occurrences.clone(),
        retrieved_gold_session_occurrences: retrieved.clone(),
        any_retrieved: can_score.then_some(!retrieved.is_empty()),
        all_retrieved: can_score.then_some(retrieved.len() == occurrences.len()),
    })
}

fn selected_overlap_bytes(
    reference: &str,
    target_start: usize,
    target_end: usize,
    evidence: &[EvidenceCitation],
) -> usize {
    let mut ranges: Vec<_> = evidence
        .iter()
        .filter(|item| item.source.reference == reference)
        .filter_map(|item| {
            let start = item.source.start.max(target_start);
            let end = item.source.end.min(target_end);
            (start < end).then_some((start, end))
        })
        .collect();
    ranges.sort_unstable();
    let mut total = 0usize;
    let mut current: Option<(usize, usize)> = None;
    for (start, end) in ranges {
        match current {
            Some((from, to)) if start <= to => current = Some((from, to.max(end))),
            Some((from, to)) => {
                total += to - from;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((from, to)) = current {
        total += to - from;
    }
    total
}

fn full_history_retrieval(
    evidence: &[EvidenceCitation],
    sources: &[SessionSource],
) -> FullHistorySessionRetrieval {
    let known: BTreeMap<&str, &SessionSource> = sources
        .iter()
        .map(|source| (source.reference.as_str(), source))
        .collect();
    let mut selected_indices = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for citation in evidence {
        if citation.source.reference.starts_with("session://") {
            if let Some(source) = known.get(citation.source.reference.as_str()) {
                selected_indices.insert(source.session_index);
            } else {
                unresolved.insert(citation.source.reference.clone());
            }
        }
    }
    let selected_occurrences: Vec<_> = sources
        .iter()
        .filter(|source| selected_indices.contains(&source.session_index))
        .map(|source| SessionOccurrence {
            session_id: source.upstream_session_id.clone(),
            session_index: source.session_index,
        })
        .collect();
    let selected_ids: BTreeSet<_> = selected_occurrences
        .iter()
        .map(|occurrence| occurrence.session_id.clone())
        .collect();
    let total = sources.len();
    FullHistorySessionRetrieval {
        total_history_sessions: total,
        selected_history_sessions: selected_occurrences.len(),
        selected_fraction: if total == 0 {
            0.0
        } else {
            selected_occurrences.len() as f64 / total as f64
        },
        selected_session_ids: selected_ids.into_iter().collect(),
        selected_session_occurrences: selected_occurrences,
        unresolved_session_references: unresolved.into_iter().collect(),
        interpretation: "Descriptive breadth only: selected source occurrences divided by all indexed input occurrences; selected_session_ids lists distinct upstream IDs, while selected_session_occurrences preserves duplicate-ID identities. This is not a relevance or answer-session metric.",
    }
}

fn validate_selected_coordinates(
    evidence: &[EvidenceCitation],
    sources: &[SessionSource],
) -> anyhow::Result<()> {
    let by_reference: BTreeMap<String, &SessionSource> = sources
        .iter()
        .map(|source| (source.reference.clone(), source))
        .collect();
    for citation in evidence {
        let source = by_reference
            .get(citation.source.reference.as_str())
            .with_context(|| {
                format!(
                    "selected source reference is outside the full input history: {}",
                    citation.source.reference
                )
            })?;
        ensure!(
            citation.source.view == "episode-utf8/1",
            "unexpected source view for {}",
            citation.source.reference
        );
        ensure!(
            citation.source.revision == source.revision
                && citation.source.view_digest.as_deref() == Some(source.revision.as_str()),
            "selected source revision does not match the indexed session"
        );
        ensure!(
            citation.source.start <= citation.source.end
                && citation.source.end <= source.body.len()
                && source.body.is_char_boundary(citation.source.start)
                && source.body.is_char_boundary(citation.source.end),
            "selected source coordinate is outside its exact UTF-8 session body"
        );
        ensure!(
            citation.support.contains(&source.episode_id),
            "selected citation does not name its source episode in support"
        );
    }
    Ok(())
}

fn format_session(session: &InputSession) -> anyhow::Result<(String, Vec<TurnRange>)> {
    let mut body = format!("[session_date={}]\n", session.date);
    let mut turn_ranges = Vec::with_capacity(session.turns.len());
    for (turn_index, turn) in session.turns.iter().enumerate() {
        ensure!(!turn.role.is_empty(), "source turn has an empty role");
        body.push_str(&format!("[turn_index={turn_index} role={}]\n", turn.role));
        let start = body.len();
        body.push_str(&turn.content);
        let end = body.len();
        turn_ranges.push(TurnRange {
            turn_index,
            role: turn.role.clone(),
            start,
            end,
        });
        body.push('\n');
    }
    Ok((body, turn_ranges))
}

fn validate_input_cases(cases: &[InputCase]) -> anyhow::Result<()> {
    let mut case_ids = BTreeSet::new();
    for case in cases {
        ensure!(!case.case_id.is_empty(), "input case id is empty");
        ensure!(
            case_ids.insert(case.case_id.as_str()),
            "duplicate input case id"
        );
        ensure!(!case.question.is_empty(), "input question is empty");
        ensure!(!case.question_date.is_empty(), "question date is empty");
        let occurrence_ids = indexed_session_ids(&case.history_sessions);
        ensure!(
            occurrence_ids.iter().collect::<BTreeSet<_>>().len() == occurrence_ids.len(),
            "source occurrence identities are not unique in case {}",
            case.case_id
        );
        for session in &case.history_sessions {
            ensure!(
                !session.session_id.is_empty() && !session.turns.is_empty(),
                "empty source session in case {}",
                case.case_id
            );
            timestamp_from_source_date(&session.date)
                .with_context(|| format!("invalid source date in case {}", case.case_id))?;
            for turn in &session.turns {
                ensure!(!turn.role.is_empty(), "source turn has an empty role");
            }
        }
    }
    Ok(())
}

/// Give duplicate upstream IDs unique per-input identities. Unique IDs retain
/// their original identity, while every repeated occurrence gets a stable
/// ordinal suffix. Candidate IDs are checked against every original ID to
/// avoid colliding with a legitimate upstream identifier.
fn indexed_session_ids(sessions: &[InputSession]) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut used: BTreeSet<String> = sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect();
    for session in sessions {
        *counts.entry(&session.session_id).or_default() += 1;
    }

    sessions
        .iter()
        .enumerate()
        .map(|(session_index, session)| {
            if counts[session.session_id.as_str()] == 1 {
                return session.session_id.clone();
            }
            let base = format!(
                "{}#longmemeval-occurrence-{session_index}",
                session.session_id
            );
            let mut candidate = base.clone();
            let mut suffix = 1usize;
            while used.contains(&candidate) {
                candidate = format!("{base}-{suffix}");
                suffix += 1;
            }
            used.insert(candidate.clone());
            candidate
        })
        .collect()
}

fn gold_answer_occurrences(
    answer_session_ids: &[String],
    annotations: &[AnnotatedTurn],
    sources: &[SessionSource],
) -> anyhow::Result<Vec<SessionOccurrence>> {
    let mut seen_turns = BTreeSet::new();
    for annotation in annotations {
        ensure!(
            seen_turns.insert((annotation.session_index, annotation.turn_index)),
            "duplicate annotated answer turn"
        );
        let source = sources
            .get(annotation.session_index)
            .context("annotated answer session index missing")?;
        ensure!(
            source.upstream_session_id == annotation.session_id,
            "annotated answer session ID does not match input index"
        );
        ensure!(
            source
                .turn_ranges
                .iter()
                .any(|turn| turn.turn_index == annotation.turn_index),
            "annotated answer turn index missing"
        );
    }

    let mut seen_ids = BTreeSet::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut occurrences = Vec::new();
    for session_id in answer_session_ids {
        ensure!(
            seen_ids.insert(session_id.as_str()),
            "duplicate gold answer session ID {:?}",
            session_id
        );
        let matching: Vec<_> = sources
            .iter()
            .filter(|source| source.upstream_session_id.as_str() == session_id.as_str())
            .collect();
        match matching.as_slice() {
            [source] => {
                let occurrence = SessionOccurrence {
                    session_id: source.upstream_session_id.clone(),
                    session_index: source.session_index,
                };
                if seen_occurrences.insert(occurrence.clone()) {
                    occurrences.push(occurrence);
                }
            }
            [] => {}
            _ => {
                let indices: BTreeSet<_> = annotations
                    .iter()
                    .filter(|annotation| annotation.session_id.as_str() == session_id.as_str())
                    .map(|annotation| annotation.session_index)
                    .collect();
                ensure!(
                    !indices.is_empty(),
                    "ambiguous gold session ID {:?}: it occurs at input indices {:?}, but annotated_answer_turns has no matching session_index",
                    session_id,
                    matching
                        .iter()
                        .map(|source| source.session_index)
                        .collect::<Vec<_>>()
                );
                for session_index in indices {
                    let source = sources
                        .get(session_index)
                        .context("gold occurrence index missing from captured sources")?;
                    ensure!(
                        source.upstream_session_id.as_str() == session_id.as_str(),
                        "gold occurrence index conflicts with upstream session ID {:?}",
                        session_id
                    );
                    let occurrence = SessionOccurrence {
                        session_id: source.upstream_session_id.clone(),
                        session_index,
                    };
                    if seen_occurrences.insert(occurrence.clone()) {
                        occurrences.push(occurrence);
                    }
                }
            }
        }
    }
    Ok(occurrences)
}

fn timestamp_from_source_date(value: &str) -> anyhow::Result<Timestamp> {
    let mut parts = value.split_whitespace();
    let date = parts.next().context("session date missing YYYY/MM/DD")?;
    let time = parts.last().context("session time missing HH:MM")?;
    let mut date_parts = date.split('/');
    let year: i64 = date_parts.next().context("year missing")?.parse()?;
    let month: i64 = date_parts.next().context("month missing")?.parse()?;
    let day: i64 = date_parts.next().context("day missing")?.parse()?;
    ensure!(date_parts.next().is_none(), "invalid source date shape");
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next().context("hour missing")?.parse()?;
    let minute: i64 = time_parts.next().context("minute missing")?.parse()?;
    ensure!(time_parts.next().is_none(), "invalid source time shape");
    ensure!((1..=12).contains(&month), "source month out of range");
    ensure!((0..=23).contains(&hour), "source hour out of range");
    ensure!((0..=59).contains(&minute), "source minute out of range");
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    ensure!(
        (1..=month_days[(month - 1) as usize]).contains(&day),
        "source day out of range"
    );

    // Howard Hinnant's civil-date conversion; all source dates share one timezone.
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    Ok(Timestamp(
        days_since_epoch * 86_400_000 + hour * 3_600_000 + minute * 60_000,
    ))
}

fn aggregate_by_budget(cases: &[CaseReport]) -> BTreeMap<usize, BudgetAggregate> {
    BUDGETS
        .into_iter()
        .map(|budget| {
            let query_rows: Vec<_> = cases
                .iter()
                .flat_map(|case| {
                    case.queries
                        .iter()
                        .filter(move |query| query.budget == budget)
                })
                .collect();
            let answerable_cases = cases
                .iter()
                .filter(|case| !case.annotated_abstention)
                .count();
            let abstention_cases = cases
                .iter()
                .filter(|case| case.annotated_abstention)
                .count();
            let mut answer_turn_annotated_bytes = 0;
            let mut answer_turn_retrieved_bytes = 0;
            let mut answer_session_measured_cases = 0;
            let mut any_answer_session_retrieved_cases = 0;
            let mut all_answer_sessions_retrieved_cases = 0;
            let mut history_fraction_total = 0.0;
            for query in &query_rows {
                if let Some(coverage) = &query.answer_turn_byte_coverage {
                    answer_turn_annotated_bytes += coverage.total_annotated_bytes;
                    answer_turn_retrieved_bytes += coverage.retrieved_annotated_bytes;
                }
                if let Some(answer_sessions) = &query.answer_session_retrieval
                    && answer_sessions.status == "measured"
                {
                    answer_session_measured_cases += 1;
                    any_answer_session_retrieved_cases +=
                        usize::from(answer_sessions.any_retrieved == Some(true));
                    all_answer_sessions_retrieved_cases +=
                        usize::from(answer_sessions.all_retrieved == Some(true));
                }
                history_fraction_total += query.full_history_session_retrieval.selected_fraction;
            }
            let count = query_rows.len();
            (
                budget,
                BudgetAggregate {
                    answerable_cases,
                    abstention_cases,
                    answer_turn_annotated_bytes,
                    answer_turn_retrieved_bytes,
                    weighted_answer_turn_byte_coverage: (answer_turn_annotated_bytes > 0)
                        .then_some(
                            answer_turn_retrieved_bytes as f64 / answer_turn_annotated_bytes as f64,
                        ),
                    answer_session_measured_cases,
                    any_answer_session_retrieved_cases,
                    all_answer_sessions_retrieved_cases,
                    mean_full_history_session_selection_fraction: (count > 0)
                        .then_some(history_fraction_total / count as f64),
                    abstention_scoring: "not scored as success without a reader",
                },
            )
        })
        .collect()
}

fn dataset_profile(choice: Option<&str>) -> anyhow::Result<&'static DatasetProfile> {
    match choice.unwrap_or("v1") {
        "v1" => Ok(&V1_PROFILE),
        "v2" => Ok(&V2_PROFILE),
        "v3" => Ok(&V3_PROFILE),
        other => {
            anyhow::bail!("unsupported OXIBRAIN_LONGMEMEVAL_SUBSET {other:?}; choose v1, v2, or v3")
        }
    }
}

fn read_policy_for_render_mode(mode: &str) -> anyhow::Result<ReadPolicy> {
    let mut policy = ReadPolicy::default();
    match mode {
        // Leave the constructor's production policy intact. This deliberately
        // follows a future default change without silently pinning old behavior.
        "default" => {}
        "compact" => policy.compact_evidence = true,
        "full" => policy.compact_evidence = false,
        other => anyhow::bail!(
            "unsupported OXIBRAIN_LONGMEMEVAL_RENDER_MODE {other:?}; choose default, compact, or full"
        ),
    }
    policy.validate()?;
    Ok(policy)
}

fn validate_manifest(manifest: &Value, profile: &DatasetProfile) -> anyhow::Result<()> {
    ensure!(manifest["schema"] == SCHEMA, "unexpected dataset schema");
    ensure!(
        manifest["revision"] == DATASET_REVISION,
        "unexpected upstream dataset revision"
    );
    ensure!(
        manifest["upstream_file_sha256"] == UPSTREAM_DATA_SHA256,
        "unexpected upstream corpus hash"
    );
    ensure!(
        manifest["selected_count"] == profile.cases,
        "unexpected selected case count"
    );
    ensure!(
        manifest["input"]["sha256"] == profile.input_sha256,
        "manifest input hash mismatch"
    );
    ensure!(
        manifest["gold"]["sha256"] == profile.gold_sha256,
        "manifest gold hash mismatch"
    );
    ensure!(
        manifest["preparation_script"]["sha256"] == profile.freezer_sha256,
        "manifest preparation script hash mismatch"
    );
    ensure!(
        manifest["license_sha256"] == LICENSE_SHA256,
        "manifest license hash mismatch"
    );
    ensure!(
        manifest["input"]["path"] == profile.manifest_input_path,
        "unexpected input filename"
    );
    ensure!(
        manifest["gold"]["path"] == profile.manifest_gold_path,
        "unexpected gold filename"
    );
    ensure!(
        manifest["input"]["rows"] == profile.cases && manifest["gold"]["rows"] == profile.cases,
        "unexpected manifest row count"
    );
    ensure!(
        manifest["source_rows_scanned"] == 500,
        "unexpected source scan count"
    );
    Ok(())
}

fn parse_jsonl<T: for<'de> Deserialize<'de>>(bytes: &[u8], label: &str) -> anyhow::Result<Vec<T>> {
    let text = std::str::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("invalid JSON in {label}")))
        .collect()
}

fn required_path(name: &str) -> anyhow::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} must be set explicitly"))
}

fn ensure_eq_digest(bytes: &[u8], expected: &str, label: &str) -> anyhow::Result<()> {
    let actual = sha256_hex(bytes);
    ensure!(actual == expected, "{label} checksum mismatch: {actual}");
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_blake3(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn check_optional_pin(name: &str, actual: &str) -> anyhow::Result<()> {
    if let Ok(expected) = std::env::var(name) {
        ensure!(
            expected.eq_ignore_ascii_case(actual),
            "{name} does not match the loaded local artifact"
        );
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn manifest_fixture(profile: &DatasetProfile) -> Value {
    serde_json::json!({
        "schema": SCHEMA,
        "revision": DATASET_REVISION,
        "upstream_file_sha256": UPSTREAM_DATA_SHA256,
        "selected_count": profile.cases,
        "input": {
            "sha256": profile.input_sha256,
            "path": profile.manifest_input_path,
            "rows": profile.cases,
        },
        "gold": {
            "sha256": profile.gold_sha256,
            "path": profile.manifest_gold_path,
            "rows": profile.cases,
        },
        "preparation_script": { "sha256": profile.freezer_sha256 },
        "license_sha256": LICENSE_SHA256,
        "source_rows_scanned": 500,
    })
}

#[test]
fn dataset_profiles_default_to_v1_and_keep_distinct_pins() {
    let default = dataset_profile(None).expect("default profile");
    let explicit_v1 = dataset_profile(Some("v1")).expect("v1 profile");
    let v2 = dataset_profile(Some("v2")).expect("v2 profile");
    let v3 = dataset_profile(Some("v3")).expect("v3 profile");

    assert_eq!(default.name, "v1");
    assert_eq!(explicit_v1.name, "v1");
    assert_eq!(v2.name, "v2");
    assert_eq!(default.cases, 20);
    assert_eq!(default.full_history_sessions, 943);
    assert_eq!(v2.cases, 20);
    assert_eq!(v2.full_history_sessions, 980);
    assert_eq!(v3.cases, 20);
    assert_eq!(v3.full_history_sessions, 931);
    assert_ne!(default.input_sha256, v2.input_sha256);
    assert_ne!(v2.input_sha256, v3.input_sha256);
    assert_ne!(default.gold_sha256, v2.gold_sha256);
    assert_ne!(v2.gold_sha256, v3.gold_sha256);
    assert_ne!(default.manifest_sha256, v2.manifest_sha256);
    assert_ne!(v2.manifest_sha256, v3.manifest_sha256);
    assert!(dataset_profile(Some("v4")).is_err());
}

#[test]
fn duplicate_upstream_ids_keep_ordinal_occurrences_and_gold_indices() {
    let duplicate = InputSession {
        session_id: "session-duplicate".into(),
        date: "2025/02/03 (Mon) 04:05".into(),
        turns: vec![InputTurn {
            content: "same source bytes".into(),
            role: "assistant".into(),
        }],
    };
    let input = InputCase {
        case_id: "case-duplicate".into(),
        question: "synthetic question".into(),
        question_date: "2025/02/04".into(),
        history_sessions: vec![duplicate.clone(), duplicate],
    };
    validate_input_cases(std::slice::from_ref(&input))
        .expect("duplicate source IDs pass ordinal structural preflight");
    let indexed_ids = indexed_session_ids(&input.history_sessions);
    assert_ne!(indexed_ids[0], indexed_ids[1]);
    assert!(indexed_ids[0].contains("occurrence-0"));
    assert!(indexed_ids[1].contains("occurrence-1"));

    let gold = GoldCase {
        case_id: input.case_id.clone(),
        question_type: "single-session-user".into(),
        abstention: false,
        answer: Value::Null,
        answer_session_ids: vec!["session-duplicate".into()],
        annotated_answer_turns: vec![
            AnnotatedTurn {
                session_id: "session-duplicate".into(),
                session_index: 0,
                turn_index: 0,
            },
            AnnotatedTurn {
                session_id: "session-duplicate".into(),
                session_index: 1,
                turn_index: 0,
            },
        ],
    };
    let sources = test_session_sources(&input.history_sessions);
    let occurrences = gold_answer_occurrences(
        &gold.answer_session_ids,
        &gold.annotated_answer_turns,
        &sources,
    )
    .expect("both independently annotated occurrences map");
    assert_eq!(
        occurrences,
        vec![
            SessionOccurrence {
                session_id: "session-duplicate".into(),
                session_index: 0,
            },
            SessionOccurrence {
                session_id: "session-duplicate".into(),
                session_index: 1,
            },
        ]
    );

    let ambiguous = GoldCase {
        annotated_answer_turns: Vec::new(),
        ..gold
    };
    let error = gold_answer_occurrences(
        &ambiguous.answer_session_ids,
        &ambiguous.annotated_answer_turns,
        &sources,
    )
    .expect_err("ambiguous gold fails");
    assert!(error.to_string().contains("ambiguous gold session ID"));
}

#[test]
fn unique_upstream_session_ids_keep_their_existing_identity() {
    let sessions = vec![test_session("unique-one"), test_session("unique-two")];
    assert_eq!(
        indexed_session_ids(&sessions),
        vec!["unique-one".to_string(), "unique-two".to_string()]
    );
}

#[test]
fn retrieval_counts_duplicate_session_occurrences_independently() {
    let sessions = vec![test_session("same-id"), test_session("same-id")];
    let sources = test_session_sources(&sessions);
    let evidence = vec![test_citation(&sources[1].reference)];
    let retrieval = full_history_retrieval(&evidence, &sources);

    assert_eq!(retrieval.total_history_sessions, 2);
    assert_eq!(retrieval.selected_history_sessions, 1);
    assert_eq!(retrieval.selected_session_ids, vec!["same-id".to_string()]);
    assert_eq!(
        retrieval.selected_session_occurrences,
        vec![SessionOccurrence {
            session_id: "same-id".into(),
            session_index: 1,
        }]
    );
}

fn test_session(session_id: &str) -> InputSession {
    InputSession {
        session_id: session_id.into(),
        date: "2025/02/03 (Mon) 04:05".into(),
        turns: vec![InputTurn {
            content: "synthetic source".into(),
            role: "assistant".into(),
        }],
    }
}

fn test_session_sources(sessions: &[InputSession]) -> Vec<SessionSource> {
    let indexed_ids = indexed_session_ids(sessions);
    sessions
        .iter()
        .enumerate()
        .map(|(session_index, session)| {
            let (body, turn_ranges) = format_session(session).expect("valid test session");
            let indexed_id = &indexed_ids[session_index];
            let revision = oxibrain_core::id::content_hash(&body).hex().to_uppercase();
            SessionSource {
                upstream_session_id: session.session_id.clone(),
                session_index,
                reference: format!("session://{indexed_id}"),
                revision,
                episode_id: format!("episode-{session_index}"),
                body,
                turn_ranges,
            }
        })
        .collect()
}

fn test_citation(reference: &str) -> EvidenceCitation {
    EvidenceCitation {
        citation_id: String::new(),
        unit_id: "unit".into(),
        layer_index: 0,
        rendered_start: 0,
        rendered_end: 0,
        rendered_text_start: 0,
        rendered_text_end: 0,
        source: SourceCoordinate {
            reference: reference.into(),
            revision: "revision".into(),
            view: "episode-utf8/1".into(),
            view_digest: None,
            decoder: None,
            start: 0,
            end: 0,
            document_uuid: None,
        },
        role: "untrusted_observation".into(),
        support: Vec::new(),
        shortened: false,
        omitted_ranges: Vec::new(),
    }
}

#[test]
fn render_mode_default_preserves_constructor_policy_and_overrides_are_explicit() {
    assert_eq!(
        read_policy_for_render_mode("default")
            .expect("default policy")
            .compact_evidence,
        ReadPolicy::default().compact_evidence
    );
    assert!(
        read_policy_for_render_mode("compact")
            .expect("compact policy")
            .compact_evidence
    );
    assert!(
        !read_policy_for_render_mode("full")
            .expect("full policy")
            .compact_evidence
    );
    assert!(read_policy_for_render_mode("other").is_err());
}

#[test]
fn each_profile_accepts_only_its_manifest_paths_and_pins() {
    let v1 = manifest_fixture(&V1_PROFILE);
    let v2 = manifest_fixture(&V2_PROFILE);
    let v3 = manifest_fixture(&V3_PROFILE);
    validate_manifest(&v1, &V1_PROFILE).expect("v1 manifest fixture validates");
    validate_manifest(&v2, &V2_PROFILE).expect("v2 manifest fixture validates");
    validate_manifest(&v3, &V3_PROFILE).expect("v3 manifest fixture validates");
    assert!(validate_manifest(&v1, &V2_PROFILE).is_err());
    assert!(validate_manifest(&v2, &V1_PROFILE).is_err());
    assert!(validate_manifest(&v1, &V3_PROFILE).is_err());
    assert!(validate_manifest(&v2, &V3_PROFILE).is_err());
    assert!(validate_manifest(&v3, &V1_PROFILE).is_err());
    assert!(validate_manifest(&v3, &V2_PROFILE).is_err());
}

#[test]
fn source_turn_byte_ranges_are_utf8_exact_and_merge_overlapping_evidence() {
    let session = InputSession {
        session_id: "session-1".into(),
        date: "2025/02/03 (Mon) 04:05".into(),
        turns: vec![InputTurn {
            content: "안녕 🌱".into(),
            role: "assistant".into(),
        }],
    };
    let (body, ranges) = format_session(&session).expect("format session");
    let turn = &ranges[0];
    assert_eq!(&body[turn.start..turn.end], "안녕 🌱");
    assert_eq!(turn.end - turn.start, "안녕 🌱".len());
    let citation = |start, end| EvidenceCitation {
        citation_id: String::new(),
        unit_id: "unit".into(),
        layer_index: 0,
        rendered_start: 0,
        rendered_end: 0,
        rendered_text_start: 0,
        rendered_text_end: 0,
        source: SourceCoordinate {
            reference: "session://session-1".into(),
            revision: "rev".into(),
            view: "episode-utf8/1".into(),
            view_digest: None,
            decoder: None,
            start,
            end,
            document_uuid: None,
        },
        role: "untrusted_observation".into(),
        support: Vec::new(),
        shortened: false,
        omitted_ranges: Vec::new(),
    };
    let evidence = [
        citation(turn.start, turn.start + "안녕 ".len()),
        citation(turn.start + "안녕".len(), turn.end),
    ];
    assert_eq!(
        selected_overlap_bytes("session://session-1", turn.start, turn.end, &evidence),
        turn.end - turn.start
    );
}

#[test]
fn source_date_parser_orders_longmemeval_wall_clock_values() {
    let timestamp = timestamp_from_source_date("2024/02/29 (Thu) 23:45").expect("valid date");
    let next = timestamp_from_source_date("2024/03/01 (Fri) 00:00").expect("valid next date");
    assert_eq!(next.0 - timestamp.0, 15 * 60_000);
    assert!(timestamp_from_source_date("2023/02/29 (Wed) 12:00").is_err());
}

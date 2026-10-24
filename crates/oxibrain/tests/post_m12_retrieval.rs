//! Desired-behavior regressions for the first two post-M12 retrieval defects.
use oxibrain::{
    Brain, BrainConfig, DocumentRootSpec, IndexOptions, ReadScope, RecallRequest, SearchRequest,
};
use oxibrain_core::context::render_context;
use oxibrain_core::retrieval::{QueryMode, SearchPlane};
use oxibrain_ports::{EmbeddingPort, TokenizerPort};
use std::sync::Arc;

struct Bytes;
impl TokenizerPort for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
    fn truncate_to(&self, text: &str, n: usize) -> String {
        text.chars().take(n).collect()
    }
    fn id(&self) -> &str {
        "test-byte-tokenizer"
    }
    fn quality(&self) -> oxibrain_ports::tokenizer::TokenizerQuality {
        oxibrain_ports::tokenizer::TokenizerQuality::Exact
    }
}
fn envelope(profile: &str, version: u8, html: bool) -> String {
    let yaml = format!(
        "---\nformat: pdc-document/{version}\nbody: {profile}\nid: 018f47c6-4a77-7c52-9db8-000000000001\ncreated: 2026-09-14T12:34:56.789Z\nupdated: 2026-09-14T12:34:56.789Z\ntitle: Fixture\n---\n"
    );
    if html {
        format!("<!--\n{yaml}-->\n")
    } else {
        yaml
    }
}
async fn root(brain: &Brain, path: &std::path::Path, alias: &str, space: &str) {
    brain
        .register_document_root(DocumentRootSpec {
            alias: alias.into(),
            path: path.into(),
            space: space.into(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        })
        .await
        .unwrap();
}
fn query(space: &str, mode: QueryMode) -> SearchRequest {
    SearchRequest {
        text: "OrionRecovery".into(),
        scope: ReadScope::current(space),
        planes: [SearchPlane::Documents].into_iter().collect(),
        mode,
        limit: 1,
    }
}
fn recall(space: &str, budget: usize) -> RecallRequest {
    let mut r = RecallRequest::new(space, "OrionRecovery", budget);
    r.planes = [SearchPlane::Documents].into_iter().collect();
    r.require_exact_tokens = true;
    r
}

#[tokio::test]
async fn heading_recall_includes_answer_and_condition_across_readable_profiles() {
    let markdown = "# OrionRecovery\n\nApproved rollback command: restore-release-53.\n\nDo not run unless the health check has failed.\n\n## Verification\n\nCheck the restored release.\n\n# OtherRecovery\n\nUNRELATED-SECTION\n";
    let html = "<h1>OrionRecovery</h1><p>Approved rollback command: restore-release-53.</p><p>Do not run unless the health check has failed.</p><h2>Verification</h2><p>Check the restored release.</p><h1>OtherRecovery</h1><p>UNRELATED-SECTION</p>";
    let cases = [
        (
            "md",
            format!("{}{markdown}", envelope("pdc-markdown/1", 2, false)),
        ),
        ("html", format!("{}{html}", envelope("pdc-html/1", 2, true))),
        (
            "djot",
            format!("{}{markdown}", envelope("pdc-djot/1", 1, false)),
        ),
        ("html", format!("{}{html}", envelope("pdc-html/1", 1, true))),
        ("md", markdown.into()),
        ("html", html.into()),
    ];
    for (ext, source) in cases {
        let dir = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let path = vault.path().join(format!("note.{ext}"));
        std::fs::write(&path, &source).unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path()))
            .await
            .unwrap()
            .with_tokenizer(Arc::new(Bytes));
        let space = brain.ensure_space("fixture").await.unwrap();
        root(&brain, vault.path(), "vault", "fixture").await;
        let search = brain
            .search_with_scope(query(&space, QueryMode::Lexical))
            .await
            .unwrap();
        assert_eq!(search.data.documents.len(), 1, "{ext}");
        let result = brain
            .recall_with_request(recall(&space, 2000))
            .await
            .unwrap();
        let rendered = render_context(&result.data.context);
        assert!(rendered.contains("restore-release-53"), "{ext}: {rendered}");
        assert!(rendered.contains("Do not run unless the health check has failed."));
        assert!(rendered.contains("Check the restored release."));
        assert_eq!(rendered.len(), result.data.context.total_tokens);
        assert!(rendered.len() <= 2000);
        let decoded = match ext {
            "md" if source.starts_with("---") => {
                oxibrain_connectors::parse_markdown_document("", source.as_bytes())
                    .unwrap()
                    .body
                    .text
            }
            "html" if source.starts_with("<!--") => {
                oxibrain_connectors::parse_html_document("", source.as_bytes())
                    .unwrap()
                    .body
                    .text
            }
            "djot" => {
                oxibrain_connectors::parse_djot_document("", source.as_bytes())
                    .unwrap()
                    .body
                    .text
            }
            "html" => oxibrain_connectors::html_to_text(&source),
            _ => source.clone(),
        };
        // P10: a peer section can hold an independent condition. Check the
        // target evidence's coordinates instead of treating ranking as proof
        // that every other peer is irrelevant.
        let citation = result
            .data
            .evidence
            .iter()
            .find(|citation| {
                decoded[citation.source.start..citation.source.end].contains("restore-release-53")
            })
            .unwrap();
        let span = &decoded[citation.source.start..citation.source.end];
        assert!(span.contains("restore-release-53") && span.contains("Do not run unless"));
        assert!(!span.contains("UNRELATED-SECTION"));
        assert_eq!(
            citation.source.view_digest.as_deref(),
            Some(blake3::hash(decoded.as_bytes()).to_hex().as_str())
        );
        assert_eq!(
            citation.source.document_uuid.is_some(),
            source.starts_with("---") || source.starts_with("<!--")
        );
        assert!(!citation.shortened);
        assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
    }
}

#[tokio::test]
async fn long_section_has_complete_support_or_an_explicit_budget_excerpt() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let source = format!(
        "# OrionRecovery\n\nApproved rollback command: restore-release-53.\n\n{}\n\nDo not run unless the health check has failed.\n\n# OtherRecovery\n\nUNRELATED-SECTION",
        "Long operational context. ".repeat(220)
    );
    std::fs::write(vault.path().join("note.md"), &source).unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes));
    let space = brain.ensure_space("fixture").await.unwrap();
    root(&brain, vault.path(), "vault", "fixture").await;
    let search = brain
        .search_with_scope(query(&space, QueryMode::Lexical))
        .await
        .unwrap();
    assert!(
        !search.data.documents[0]
            .text
            .text
            .contains("restore-release-53")
    );
    let full = brain
        .recall_with_request(recall(&space, 8000))
        .await
        .unwrap();
    let text = render_context(&full.data.context);
    assert!(text.contains("restore-release-53") && text.contains("Do not run unless"));
    let target_citation = full
        .data
        .evidence
        .iter()
        .find(|citation| {
            source[citation.source.start..citation.source.end].contains("restore-release-53")
        })
        .unwrap();
    assert!(
        !source[target_citation.source.start..target_citation.source.end]
            .contains("UNRELATED-SECTION")
    );
    let small = brain
        .recall_with_request(recall(&space, 450))
        .await
        .unwrap();
    assert!(render_context(&small.data.context).len() <= 450);
    assert!(!small.data.evidence.is_empty());
    let target_citation = small
        .data
        .evidence
        .iter()
        .find(|citation| {
            source[citation.source.start..citation.source.end].contains("OrionRecovery")
        })
        .unwrap();
    assert!(target_citation.shortened);
    assert!(!target_citation.omitted_ranges.is_empty());
}

#[tokio::test]
async fn natural_questions_find_bounded_terms_and_return_the_complete_matching_section() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let english = "# Recovery protocol\nUse Kestrel-841 after the validation check fails.\n## Guard\nProceed only when the health-check status is red.\n# Archive\nKestrel-842 is for routine release work.\n";
    let korean = "# 복구 절차\n검증 단계가 실패했을 때 Birch-26을 실행한다.\n## 조건\n상태가 빨간색일 때만 진행한다.\n# 이전 절차\nBirch-25는 점검 완료 후에만 사용한다.\n";
    std::fs::write(vault.path().join("english.md"), english).unwrap();
    std::fs::write(vault.path().join("korean.md"), korean).unwrap();

    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes));
    let space = brain.ensure_space("fixture").await.unwrap();
    root(&brain, vault.path(), "vault", "fixture").await;

    for (question, expected_locator, command, condition, excluded) in [
        (
            "Which recovery command applies when the health-check reports a failure?",
            "english.md",
            "Kestrel-841",
            "Proceed only when the health-check status is red.",
            "Kestrel-842",
        ),
        (
            "검증 단계가 실패하면 어떤 Birch-26 복구 절차를 사용해야 하나요?",
            "korean.md",
            "Birch-26",
            "상태가 빨간색일 때만 진행한다.",
            "Birch-25",
        ),
    ] {
        let search = brain
            .search_with_scope(SearchRequest {
                text: question.into(),
                scope: ReadScope::current(&space),
                planes: [SearchPlane::Documents].into_iter().collect(),
                mode: QueryMode::Lexical,
                limit: 1,
            })
            .await
            .unwrap();
        assert_eq!(search.data.documents.len(), 1, "{question}");
        assert_eq!(search.data.documents[0].locator, expected_locator);
        for channel in search
            .data
            .freshness
            .channels
            .iter()
            .filter(|channel| channel.channel == "word" || channel.channel == "ngram")
        {
            assert!(channel.eligible && channel.executed);
            assert_eq!(channel.total, None);
            assert!(channel.match_occurrences.is_some());
            assert!(channel.queries_run <= channel.query_limit.unwrap());
            assert!(channel.plan_candidates_selected <= channel.query_limit.unwrap());
            assert!(channel.plan_candidates_examined >= channel.plan_candidates_selected);
        }

        let mut request = RecallRequest::new(&space, question, 2000);
        request.planes = [SearchPlane::Documents].into_iter().collect();
        request.require_exact_tokens = true;
        let recalled = brain.recall_with_request(request).await.unwrap();
        let rendered = render_context(&recalled.data.context);
        assert!(rendered.contains(command), "{question}: {rendered}");
        assert!(rendered.contains(condition), "{question}: {rendered}");
        let source = if expected_locator == "english.md" {
            english
        } else {
            korean
        };
        let citation = recalled
            .data
            .evidence
            .iter()
            .find(|citation| {
                citation.role == "untrusted_observation"
                    && source[citation.source.start..citation.source.end].contains(command)
            })
            .unwrap();
        let cited_source = &source[citation.source.start..citation.source.end];
        assert!(cited_source.contains(command) && cited_source.contains(condition));
        assert!(!cited_source.contains(excluded));
        assert_eq!(
            citation.source.view_digest.as_deref(),
            Some(blake3::hash(source.as_bytes()).to_hex().as_str())
        );
        assert_eq!(citation.source.revision, search.data.documents[0].revision);
    }

    let dense = brain
        .search_with_scope(SearchRequest {
            text: "Which recovery command applies when the health-check reports a failure?".into(),
            scope: ReadScope::current(&space),
            planes: [SearchPlane::Documents].into_iter().collect(),
            mode: QueryMode::Dense,
            limit: 4,
        })
        .await
        .unwrap();
    assert!(dense.data.documents.is_empty());
    for channel in dense
        .data
        .freshness
        .channels
        .iter()
        .filter(|channel| channel.channel == "word" || channel.channel == "ngram")
    {
        assert!(!channel.eligible && !channel.executed);
        assert_eq!(channel.match_occurrences, None);
        assert_eq!(channel.total, None);
        assert_eq!(channel.queries_run, 0);
    }
}

#[tokio::test]
async fn recall_preserves_compared_peers_and_shared_version_conditions() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let source = "# Kestrel-841\nKestrel-841 is safe only when the health-check is red.\n# Birch-26\nBirch-26 applies only after the validation task fails.\n# Release version 5.1\nRelease version 5.1 applies only when the cluster is active.\n# Release version 5.2\nRelease version 5.2 applies only when the device is offline.";
    std::fs::write(vault.path().join("branches.md"), source).unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes));
    let space = brain.ensure_space("fixture").await.unwrap();
    root(&brain, vault.path(), "vault", "fixture").await;

    for (question, expected) in [
        (
            "Compare Kestrel-841 and Birch-26 conditions.",
            [
                "Kestrel-841 is safe only when the health-check is red.",
                "Birch-26 applies only after the validation task fails.",
            ],
        ),
        (
            "Which release version applies?",
            [
                "Release version 5.1 applies only when the cluster is active.",
                "Release version 5.2 applies only when the device is offline.",
            ],
        ),
    ] {
        let mut request = RecallRequest::new(&space, question, 2000);
        request.planes = [SearchPlane::Documents].into_iter().collect();
        request.require_exact_tokens = true;
        let recalled = brain.recall_with_request(request).await.unwrap();
        let rendered = render_context(&recalled.data.context);
        for branch in expected {
            assert!(rendered.contains(branch), "{question}: {rendered}");
            assert!(recalled.data.evidence.iter().any(|citation| {
                citation.source.reference.starts_with("doc://")
                    && source[citation.source.start..citation.source.end].contains(branch)
            }));
        }
        for citation in &recalled.data.evidence {
            if citation.source.reference.starts_with("doc://") {
                assert_eq!(
                    citation.source.view_digest.as_deref(),
                    Some(blake3::hash(source.as_bytes()).to_hex().as_str())
                );
            }
        }
    }
}

struct Vectors;
impl EmbeddingPort for Vectors {
    fn dim(&self) -> usize {
        1024
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, oxibrain::BrainError> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0; 1024];
                if text.contains("INSIDE") {
                    v[0] = 0.8;
                    v[1] = 0.6;
                } else {
                    v[0] = 1.0;
                }
                v
            })
            .collect())
    }
}
#[tokio::test]
async fn dense_and_hybrid_top_one_survives_unrelated_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let inside = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(inside.path().join("inside.md"), "INSIDE relevant evidence").unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_embedder(Arc::new(Vectors));
    let space = brain.ensure_space("requested").await.unwrap();
    root(&brain, inside.path(), "inside", "requested").await;
    for unrelated in [0, 24] {
        if unrelated > 0 {
            for i in 0..unrelated {
                std::fs::write(
                    outside.path().join(format!("outside-{i}.md")),
                    "OUTSIDE closer evidence",
                )
                .unwrap();
            }
            root(&brain, outside.path(), "outside", "other").await;
        }
        brain
            .index_documents(IndexOptions {
                embed: true,
                budget: None,
            })
            .await
            .unwrap();
        for mode in [QueryMode::Dense, QueryMode::Hybrid] {
            let response = brain.search_with_scope(query(&space, mode)).await.unwrap();
            assert_eq!(
                response.data.documents.len(),
                1,
                "{mode:?}, unrelated={unrelated}"
            );
            assert_eq!(
                response.data.documents[0].text.text,
                "INSIDE relevant evidence"
            );
            assert_eq!(response.data.documents[0].root, "inside");
            assert_eq!(response.meta.execution["partial"], false);
        }
    }
}

#[tokio::test]
async fn a_long_section_does_not_lose_its_condition_to_a_budget_quotation() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let source = format!(
        "<h1>OrionRecovery</h1><p>Use restore-release-53.</p><p>{}</p><p>Do not run unless the health check fails.</p>",
        "Operational context. ".repeat(280)
    );
    std::fs::write(vault.path().join("note.html"), &source).unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes));
    let space = brain.ensure_space("fixture").await.unwrap();
    root(&brain, vault.path(), "vault", "fixture").await;
    let search = brain
        .search_with_scope(query(&space, QueryMode::Lexical))
        .await
        .unwrap();
    assert!(
        search.data.documents[0]
            .text
            .text
            .contains("restore-release-53")
    );
    assert!(
        !search.data.documents[0]
            .text
            .text
            .contains("Do not run unless")
    );
    // The original search chunk fits, but its complete section does not.
    let result = brain
        .recall_with_request(recall(&space, 5000))
        .await
        .unwrap();
    let text = render_context(&result.data.context);
    assert!(text.len() <= 5000);
    assert!(!text.contains("restore-release-53"), "{text}");
    assert_eq!(result.meta.dropped["budget_excluded"], 1);
    assert!(result.data.context.truncated);
}

struct OrderedVectors;
impl EmbeddingPort for OrderedVectors {
    fn dim(&self) -> usize {
        1024
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, oxibrain::BrainError> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0; 1024];
                let angle = text
                    .strip_prefix("ANGLE ")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                v[0] = angle.cos();
                v[1] = angle.sin();
                v
            })
            .collect())
    }
}
#[tokio::test]
async fn recall_preserves_document_relevance_order_before_packing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(
            vault.path().join(format!("note-{i}.md")),
            format!("ANGLE {}", i as f32 / 20.0),
        )
        .unwrap();
    }
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes))
        .with_embedder(Arc::new(OrderedVectors));
    let space = brain.ensure_space("ordered").await.unwrap();
    root(&brain, vault.path(), "ordered", "ordered").await;
    brain
        .index_documents(IndexOptions {
            embed: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let search = brain
        .search_with_scope(SearchRequest {
            text: "unmatched-query".into(),
            scope: ReadScope::current(&space),
            planes: [SearchPlane::Documents].into_iter().collect(),
            mode: QueryMode::Hybrid,
            limit: 64,
        })
        .await
        .unwrap();
    assert_eq!(search.data.documents.len(), 20);
    let mut req = RecallRequest::new(&space, "unmatched-query", 1000);
    req.planes = [SearchPlane::Documents].into_iter().collect();
    let recalled = brain.recall_with_request(req).await.unwrap();
    assert!(!recalled.data.evidence.is_empty());
    let expected: Vec<_> = search
        .data
        .documents
        .iter()
        .map(|h| &h.text.provenance.reference)
        .take(recalled.data.evidence.len())
        .collect();
    let actual: Vec<_> = recalled
        .data
        .evidence
        .iter()
        .map(|e| &e.source.reference)
        .collect();
    assert_eq!(
        actual, expected,
        "database enumeration must not consume the budget before relevant hits"
    );
}

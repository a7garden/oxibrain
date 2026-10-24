use oxibrain_core::evidence_reader::*;
fn evidence() -> Vec<ReaderEvidence> {
    vec![ReaderEvidence {
        id: "E1".into(),
        text: "Use restore-9 only after the check fails.".into(),
        role: "untrusted_observation".into(),
        source: None,
    }]
}
fn reply() -> ReaderReply {
    ReaderReply {
        action: ReaderAction::Conditional,
        answer: "restore-9".into(),
        conditions: vec!["only after the check fails".into()],
        clarification: String::new(),
        citations: vec![ReaderQuote {
            id: "E1".into(),
            quote: "Use restore-9 only after the check fails.".into(),
        }],
    }
}
#[test]
fn exact_quotes_ids_and_conditions_are_required() {
    let original = reply();
    assert!(validate_delivery(&original, &evidence()).is_ok());
    for which in 0..4 {
        let mut r = reply();
        match which {
            0 => r.citations[0].id = "E2".into(),
            1 => r.citations[0].quote = "Use restore-8".into(),
            2 => r.conditions[0] = "whenever convenient".into(),
            _ => r.action = ReaderAction::Answer,
        };
        assert!(validate_delivery(&r, &evidence()).is_err());
    }
}
#[test]
fn clarification_cannot_recommend_a_command_and_abstention_has_no_answer() {
    let mut r = reply();
    r.action = ReaderAction::Clarify;
    r.clarification = "Which environment applies?".into();
    assert!(validate_delivery(&r, &evidence()).is_err());
    r.answer.clear();
    assert!(validate_delivery(&r, &evidence()).is_ok());
    r.action = ReaderAction::Abstain;
    r.conditions.clear();
    r.clarification.clear();
    r.citations.clear();
    assert!(validate_delivery(&r, &[]).is_ok());
    r.answer = "An unsupported answer".into();
    assert!(validate_delivery(&r, &[]).is_err());
}

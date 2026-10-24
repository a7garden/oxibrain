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

    let mut unknown_id = reply();
    unknown_id.citations[0].id = "E2".into();
    assert_eq!(
        validate_delivery(&unknown_id, &evidence())
            .unwrap_err()
            .errors,
        vec![ReaderDeliveryError::UnknownEvidenceId {
            index: 0,
            id: "E2".into(),
        }]
    );

    let mut invented_quote = reply();
    invented_quote.citations[0].quote = "Use restore-8".into();
    assert_eq!(
        validate_delivery(&invented_quote, &evidence())
            .unwrap_err()
            .errors,
        vec![
            ReaderDeliveryError::QuoteNotDelivered {
                index: 0,
                id: "E1".into(),
            },
            ReaderDeliveryError::ConditionNotCited { index: 0 },
        ]
    );

    let mut wrong_case = reply();
    wrong_case.citations[0].quote = "use restore-9 only after the check fails.".into();
    assert_eq!(
        validate_delivery(&wrong_case, &evidence())
            .unwrap_err()
            .errors,
        vec![ReaderDeliveryError::QuoteNotDelivered {
            index: 0,
            id: "E1".into(),
        }]
    );

    let mut paraphrased_condition = reply();
    paraphrased_condition.conditions[0] = "whenever convenient".into();
    assert_eq!(
        validate_delivery(&paraphrased_condition, &evidence())
            .unwrap_err()
            .errors,
        vec![ReaderDeliveryError::ConditionNotCited { index: 0 }]
    );

    let mut unconditional_with_condition = reply();
    unconditional_with_condition.action = ReaderAction::Answer;
    assert_eq!(
        validate_delivery(&unconditional_with_condition, &evidence())
            .unwrap_err()
            .errors,
        vec![ReaderDeliveryError::ConditionsRequireConditional]
    );
}
#[test]
fn clarification_cannot_recommend_a_command_and_abstention_has_no_answer() {
    let mut r = reply();
    r.action = ReaderAction::Clarify;
    r.clarification = "Which environment applies?".into();
    assert_eq!(
        validate_delivery(&r, &evidence()).unwrap_err().errors,
        vec![ReaderDeliveryError::AnswerMustBeEmptyForClarify]
    );
    r.answer.clear();
    assert!(validate_delivery(&r, &evidence()).is_ok());
    r.action = ReaderAction::Abstain;
    r.conditions.clear();
    r.clarification.clear();
    r.citations.clear();
    assert!(validate_delivery(&r, &[]).is_ok());
    r.answer = "An unsupported answer".into();
    assert_eq!(
        validate_delivery(&r, &[]).unwrap_err().errors,
        vec![ReaderDeliveryError::AbstentionFieldsMustBeEmpty]
    );
}

#[test]
fn conditional_cannot_move_the_answer_into_clarification() {
    let mut r = reply();
    r.answer.clear();
    r.clarification = "Which environment applies?".into();

    let errors = validate_delivery(&r, &evidence()).unwrap_err().errors;
    assert!(errors.contains(&ReaderDeliveryError::AnswerRequired {
        action: ReaderAction::Conditional,
    }));
    assert!(
        errors.contains(&ReaderDeliveryError::ClarificationMustBeEmpty {
            action: ReaderAction::Conditional,
        })
    );
}

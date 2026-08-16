//! Explainability queries: why (provenance + confidence breakdown).

use crate::sql_err;
use oxibrain_core::knowledge::{Object, Statement, TypedValue};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainBlock {
    pub statement: Statement,
    pub status: String,
    pub assertions: Vec<AssertionDetail>,
    pub confidence_breakdown: ConfidenceBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionDetail {
    pub assertion_id: String,
    pub episode_id: String,
    pub extractor: Option<String>,
    pub polarity: String,
    pub confidence: f32,
    pub recorded_at: i64,
    /// Verbatim subject-mention surface for this assertion (P3: assertions
    /// keep their mention so re-resolution stays exact). `None` when the
    /// mention row is missing (legacy rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceBreakdown {
    pub raw_confidence: f32,
    pub support_count: usize,
    pub contradiction_count: usize,
}

/// Provenance and confidence breakdown for a statement.
pub fn why(conn: &Connection, space: &str, statement_id: &str) -> Result<ExplainBlock, BrainError> {
    let stmt_row = conn
        .query_row(
            "SELECT id, subject_id, predicate, object_entity, object_literal
             FROM statements WHERE space_id = ?1 AND id = ?2",
            params![space, statement_id],
            |r| {
                let object_entity: Option<String> = r.get(3)?;
                let object_literal: Option<String> = r.get(4)?;
                let object = match (object_entity, object_literal) {
                    (Some(eid), None) => Object::Entity(eid),
                    (None, Some(lit)) => {
                        Object::Literal(serde_json::from_str(&lit).expect("valid literal in db"))
                    }
                    _ => Object::Literal(TypedValue::Text(String::new())),
                };
                Ok(Statement {
                    id: r.get(0)?,
                    space: space.to_string(),
                    subject: r.get(1)?,
                    predicate: r.get(2)?,
                    object,
                })
            },
        )
        .map_err(sql_err)?;

    let status: String = conn
        .query_row(
            "SELECT status FROM beliefs WHERE statement_id = ?1 ORDER BY valid_from DESC LIMIT 1",
            params![statement_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "unknown".to_string());

    let mut assert_stmt = conn
        .prepare(
            "SELECT a.id, a.episode_id, a.extractor_id, a.polarity, a.confidence, a.recorded_at, m.surface
             FROM assertions a
             LEFT JOIN mentions m ON m.assertion_id = a.id AND m.role = 'subject'
             WHERE a.statement_id = ?1 ORDER BY a.recorded_at",
        )
        .map_err(sql_err)?;
    let details = assert_stmt
        .query_map(params![statement_id], |r| {
            let polarity_int: i64 = r.get(3)?;
            let polarity = if polarity_int == 1 { "affirm" } else { "deny" };
            Ok(AssertionDetail {
                assertion_id: r.get(0)?,
                episode_id: r.get(1)?,
                extractor: r.get(2)?,
                polarity: polarity.to_string(),
                confidence: r.get(4)?,
                recorded_at: r.get(5)?,
                mention: r.get(6)?,
            })
        })
        .map_err(sql_err)?;
    let mut assertions = Vec::new();
    for d in details {
        assertions.push(d.map_err(sql_err)?);
    }

    let support_count = assertions.iter().filter(|a| a.polarity == "affirm").count();
    let contradiction_count = assertions.iter().filter(|a| a.polarity == "deny").count();
    let raw_confidence = assertions.first().map(|a| a.confidence).unwrap_or(0.0);

    Ok(ExplainBlock {
        statement: stmt_row,
        status,
        assertions,
        confidence_breakdown: ConfidenceBreakdown {
            raw_confidence,
            support_count,
            contradiction_count,
        },
    })
}

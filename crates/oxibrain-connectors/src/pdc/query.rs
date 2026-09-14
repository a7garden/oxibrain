//! `pdc-query/1` query-definition validation (PDC-QUERY-1.0.md).
//!
//! oxibrain never executes queries. It classifies `.base` files and fenced
//! `base` blocks as opaque, read-only query definitions, validates them
//! against the same safe-general-YAML rules as the `pdc-document/2`
//! envelope (`pdc-query/1` §3), preserves the source verbatim, and reports
//! `invalid_query` diagnostics for malformed definitions. The file always
//! remains visible and untouched (§5).
//!
//! Out of scope by design: no expression evaluation, no execution, no
//! scripts/network/process/authorization effects.

use super::PDC_MAX_QUERY_BYTES;
use super::diagnostic::{PdcDiagnostic, PdcDiagnosticCode};

fn invalid(msg: impl Into<String>) -> PdcDiagnostic {
    PdcDiagnostic::new(PdcDiagnosticCode::InvalidQuery, msg)
}

/// Validate raw `.base` file bytes (transport §3.1): UTF-8 without a BOM, at
/// most 1 MiB, single safe-general-YAML mapping under the envelope
/// restrictions. The source is preserved by the caller either way.
pub fn validate_base_query(bytes: &[u8]) -> Result<(), PdcDiagnostic> {
    super::transport::reject_bom(bytes).map_err(|_| {
        invalid(
            "query definition starts with a UTF-8 byte-order mark; it is rejected, not stripped",
        )
    })?;
    if bytes.len() > PDC_MAX_QUERY_BYTES {
        return Err(invalid(format!(
            "query definition is {} bytes; `pdc-query/1` caps `.base` files at {PDC_MAX_QUERY_BYTES} bytes (1 MiB)",
            bytes.len()
        )));
    }
    let src =
        std::str::from_utf8(bytes).map_err(|_| invalid("query definition is not valid UTF-8"))?;
    validate_base_yaml(src).map_err(invalid)
}

/// Validate the YAML text of a query definition (either transport shape,
/// §3.1/§3.2): safe-general-YAML 1.2 Core, root mapping, under the envelope
/// restrictions with `invalid_query` spellings. Depth/node budget
/// violations keep the shared `document_too_complex` code.
pub(super) fn validate_base_yaml(src: &str) -> Result<(), String> {
    // Malformed queries are reportable content problems, not parse aborts:
    // the reason string travels back to the body scan for `invalid_query`
    // reporting while the block stays visible in `text`.
    super::yaml_frontmatter::parse_safe_yaml(src, PdcDiagnosticCode::InvalidQuery)
        .map(|_| ())
        .map_err(|d| d.reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_yaml_queries() {
        validate_base_yaml("filters: 'priority == 2'\nviews:\n  - type: table\n    name: T\n")
            .unwrap();
    }

    #[test]
    fn rejects_malformed_yaml_as_invalid_query() {
        let err = validate_base_query(b"filters:\n  and: [unclosed\n").unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::InvalidQuery);
    }

    #[test]
    fn rejects_bom_and_oversize() {
        let mut bom = vec![0xef, 0xbb, 0xbf];
        bom.extend_from_slice(b"filters: 'a == 1'\n");
        assert_eq!(
            validate_base_query(&bom).unwrap_err().code,
            PdcDiagnosticCode::InvalidQuery
        );
        let big = vec![b'x'; PDC_MAX_QUERY_BYTES + 1];
        assert_eq!(
            validate_base_query(&big).unwrap_err().code,
            PdcDiagnosticCode::InvalidQuery
        );
    }

    #[test]
    fn wrong_root_type_is_invalid_query() {
        assert!(validate_base_yaml("- just\n- a list\n").is_err());
    }
}

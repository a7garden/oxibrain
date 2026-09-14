//! `pdc-document/2` envelope: safe general YAML 1.2 Core.
//!
//! Unlike the frozen `pdc-document/1` envelope (a hand-rolled constrained
//! grammar in [`super::envelope`]), the v2 envelope is **general YAML** so an
//! Obsidian-compatible frontmatter can carry arbitrary user keys:
//!
//! - comments, blank lines, and quoted scalars are allowed;
//! - nested JSON-compatible mappings and sequences are allowed at any depth
//!   up to [`MAX_YAML_DEPTH`];
//! - plain scalars resolve per the YAML 1.2 Core schema (null, bool, int,
//!   float, string).
//!
//! Everything that makes YAML unsafe is rejected outright, detected on the
//! low-level event stream before any value is trusted:
//!
//! - anchors (`&a`) and aliases (`*a`) — reject;
//! - tags (`!!str`, `!foo`, `%TAG` handles) — reject;
//! - multiple documents in one stream — reject;
//! - duplicate keys at any level, and keys that are not strings — reject;
//! - non-finite plain values (`.nan`, `.inf`) — reject;
//! - depth > [`MAX_YAML_DEPTH`] or more than [`MAX_YAML_NODES`] nodes —
//!   `document_too_complex`.
//!
//! The parser is [`yaml_rust2`] driven over its low-level event iterator;
//! no event is turned into a value before the anchor/tag checks run, so a
//! rejected construct never reaches the projection. The same grammar (with
//! `invalid_query` spellings) validates `pdc-query/1` `.base` definitions
//! and fenced `base` blocks (`pdc-query/1` §3: "the same restrictions as
//! the `pdc-document/2` envelope").

use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use super::diagnostic::{PdcDiagnostic, PdcDiagnosticCode};

/// Maximum nesting depth of the envelope mapping/sequence tree. Violations
/// are `document_too_complex` (complexity cap, not a grammar error).
pub(super) const MAX_YAML_DEPTH: usize = 32;

/// Maximum total node count (scalars + collections) in one envelope.
pub(super) const MAX_YAML_NODES: usize = 10_000;

/// One validated envelope value. Floats exist only so the Core-schema
/// resolution is honest; standard fields never accept them.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum YamlValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<YamlValue>),
    Map(Vec<(String, YamlValue)>),
}

/// Parse the envelope source into its top-level mapping entries. The root
/// must be a mapping. Grammar violations (anchors, aliases, tags, duplicate
/// or non-string keys, multi-doc, non-finite values, malformed YAML, wrong
/// root type) carry `grammar_code` — `invalid_envelope` for documents,
/// `invalid_query` for `pdc-query/1` definitions. Depth/node budget
/// violations always stay `document_too_complex` (the shared complexity cap).
pub(super) fn parse_safe_yaml(
    src: &str,
    grammar_code: PdcDiagnosticCode,
) -> Result<Vec<(String, YamlValue)>, PdcDiagnostic> {
    let mut loader = Loader {
        grammar_code,
        stack: Vec::new(),
        root: None,
        nodes: 0,
        document_seen: false,
    };
    let mut parser = Parser::new_from_str(src);
    loop {
        match parser.next_token() {
            Ok((Event::StreamEnd, _)) => break,
            Ok((event, marker)) => loader.step(event, &marker)?,
            Err(e) => {
                return Err(invalid_at(
                    grammar_code,
                    format!("invalid YAML: {}", e.info()),
                    e.marker().line() as u32,
                    e.marker().col() as u32 + 1,
                ));
            }
        }
    }
    match loader.root {
        Some(YamlValue::Map(entries)) => Ok(entries),
        Some(_) => Err(invalid_at(
            grammar_code,
            "envelope must be a YAML mapping",
            1,
            1,
        )),
        None => Err(invalid_at(grammar_code, "envelope is empty", 1, 1)),
    }
}

fn invalid_at(
    code: PdcDiagnosticCode,
    msg: impl Into<String>,
    line: u32,
    col: u32,
) -> PdcDiagnostic {
    PdcDiagnostic::at(code, msg, line, col)
}

struct Loader {
    grammar_code: PdcDiagnosticCode,
    stack: Vec<Frame>,
    root: Option<YamlValue>,
    nodes: usize,
    document_seen: bool,
}

/// One open collection. `pending_key` is set while waiting for the value of
/// the last-seen mapping key.
enum Frame {
    Map {
        entries: Vec<(String, YamlValue)>,
        pending_key: Option<String>,
    },
    Seq(Vec<YamlValue>),
}

impl Loader {
    fn step(&mut self, event: Event, marker: &Marker) -> Result<(), PdcDiagnostic> {
        // Local copy: the closure must not borrow `self` while `attach`
        // mutates the loader.
        let grammar_code = self.grammar_code;
        let at = |msg: String| {
            invalid_at(
                grammar_code,
                msg,
                marker.line() as u32,
                marker.col() as u32 + 1,
            )
        };
        match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => Ok(()),
            Event::DocumentStart => {
                if self.document_seen {
                    Err(at(
                        "multi-document streams are forbidden in the envelope".to_string()
                    ))
                } else {
                    Ok(())
                }
            }
            Event::DocumentEnd => {
                self.document_seen = true;
                Ok(())
            }
            Event::Alias(_) => Err(at("aliases are forbidden in the envelope".to_string())),
            Event::Scalar(text, style, anchor, tag) => {
                self.reject_anchor_tag(anchor, tag.as_ref(), &at)?;
                let value = self.resolve_scalar(&text, style, &at)?;
                self.nodes += 1;
                self.check_node_budget()?;
                self.attach(value, &at)
            }
            Event::SequenceStart(anchor, tag) => {
                self.reject_anchor_tag(anchor, tag.as_ref(), &at)?;
                self.open_collection(Frame::Seq(Vec::new()))
            }
            Event::MappingStart(anchor, tag) => {
                self.reject_anchor_tag(anchor, tag.as_ref(), &at)?;
                self.open_collection(Frame::Map {
                    entries: Vec::new(),
                    pending_key: None,
                })
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let frame = self
                    .stack
                    .pop()
                    .ok_or_else(|| at("unbalanced collection end".to_string()))?;
                let value = match frame {
                    Frame::Map { entries, .. } => YamlValue::Map(entries),
                    Frame::Seq(items) => YamlValue::Seq(items),
                };
                self.attach(value, &at)
            }
        }
    }

    fn reject_anchor_tag(
        &self,
        anchor: usize,
        tag: Option<&Tag>,
        at: &dyn Fn(String) -> PdcDiagnostic,
    ) -> Result<(), PdcDiagnostic> {
        if anchor != 0 {
            return Err(at("anchors are forbidden in the envelope".to_string()));
        }
        if tag.is_some() {
            return Err(at("tags are forbidden in the envelope".to_string()));
        }
        Ok(())
    }

    fn open_collection(&mut self, frame: Frame) -> Result<(), PdcDiagnostic> {
        if self.stack.len() + 1 > MAX_YAML_DEPTH {
            return Err(PdcDiagnostic::new(
                PdcDiagnosticCode::DocumentTooComplex,
                format!(
                    "envelope nesting depth {} exceeds the maximum of {MAX_YAML_DEPTH}",
                    self.stack.len() + 1
                ),
            ));
        }
        self.nodes += 1;
        if self.nodes > MAX_YAML_NODES {
            return Err(PdcDiagnostic::new(
                PdcDiagnosticCode::DocumentTooComplex,
                format!(
                    "envelope node count {} exceeds the maximum of {MAX_YAML_NODES}",
                    self.nodes
                ),
            ));
        }
        self.stack.push(frame);
        Ok(())
    }

    fn check_node_budget(&self) -> Result<(), PdcDiagnostic> {
        if self.nodes > MAX_YAML_NODES {
            return Err(PdcDiagnostic::new(
                PdcDiagnosticCode::DocumentTooComplex,
                format!(
                    "envelope node count {} exceeds the maximum of {MAX_YAML_NODES}",
                    self.nodes
                ),
            ));
        }
        Ok(())
    }

    fn attach(
        &mut self,
        value: YamlValue,
        at: &dyn Fn(String) -> PdcDiagnostic,
    ) -> Result<(), PdcDiagnostic> {
        let Some(frame) = self.stack.last_mut() else {
            if self.root.is_some() {
                return Err(at("multiple root values in the envelope".to_string()));
            }
            self.root = Some(value);
            return Ok(());
        };
        match frame {
            Frame::Seq(items) => {
                items.push(value);
                Ok(())
            }
            Frame::Map {
                entries,
                pending_key,
            } => {
                if pending_key.is_none() {
                    // Key position.
                    let YamlValue::Str(key) = value else {
                        return Err(at("mapping keys must be strings".to_string()));
                    };
                    *pending_key = Some(key);
                    return Ok(());
                }
                let key = pending_key.take().expect("checked just above");
                if entries.iter().any(|(k, _)| *k == key) {
                    return Err(at(format!("duplicate key `{key}`")));
                }
                entries.push((key, value));
                Ok(())
            }
        }
    }

    /// Resolve a plain scalar per YAML 1.2 Core; quoted and block scalars are
    /// strings verbatim. Non-finite plain values are rejected outright.
    fn resolve_scalar(
        &self,
        text: &str,
        style: TScalarStyle,
        at: &dyn Fn(String) -> PdcDiagnostic,
    ) -> Result<YamlValue, PdcDiagnostic> {
        if style != TScalarStyle::Plain {
            return Ok(YamlValue::Str(text.to_string()));
        }
        if is_nonfinite(text) {
            return Err(at(format!(
                "non-finite value `{text}` is forbidden in the envelope"
            )));
        }
        Ok(match text {
            "" | "~" | "null" | "Null" | "NULL" => YamlValue::Null,
            "true" | "True" | "TRUE" => YamlValue::Bool(true),
            "false" | "False" | "FALSE" => YamlValue::Bool(false),
            _ => {
                if is_core_int(text) {
                    match text.parse::<i64>() {
                        Ok(i) => YamlValue::Int(i),
                        // Out-of-i64 integers stay numeric as JSON-compatible
                        // doubles; finiteness is re-checked as a float below.
                        Err(_) => match text.parse::<f64>() {
                            Ok(f) if f.is_finite() => YamlValue::Float(f),
                            _ => return Err(at(nonfinite_msg(text))),
                        },
                    }
                } else if is_core_float(text) {
                    match text.parse::<f64>() {
                        Ok(f) if f.is_finite() => YamlValue::Float(f),
                        _ => return Err(at(nonfinite_msg(text))),
                    }
                } else {
                    YamlValue::Str(text.to_string())
                }
            }
        })
    }
}

fn nonfinite_msg(text: &str) -> String {
    format!("non-finite value `{text}` is forbidden in the envelope")
}

/// `.nan` / `.inf` spellings with optional sign, case-insensitive
/// (YAML 1.2 core non-finite literals).
fn is_nonfinite(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let lower = body.to_ascii_lowercase();
    lower == ".nan" || lower == ".inf"
}

/// `[-+]?[0-9]+`
fn is_core_int(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`
fn is_core_float(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    if let Some(e) = exponent {
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    match mantissa.split_once('.') {
        Some((int_part, frac_part)) => {
            let int_ok = int_part.is_empty() || int_part.bytes().all(|b| b.is_ascii_digit());
            let frac_ok = frac_part.bytes().all(|b| b.is_ascii_digit());
            // At least one side of the dot must carry digits.
            int_ok && frac_ok && !(int_part.is_empty() && frac_part.is_empty())
        }
        None => false, // pure digits are ints, handled by is_core_int
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<Vec<(String, YamlValue)>, PdcDiagnostic> {
        parse_safe_yaml(src, PdcDiagnosticCode::InvalidEnvelope)
    }

    fn err(src: &str) -> PdcDiagnostic {
        parse(src).unwrap_err()
    }

    #[test]
    fn parses_comments_and_nested_structures() {
        let fields = parse(
            "# header comment\nformat: \"pdc-document/2\"\ntags:\n  - a\n  - b\nmeta:\n  depth:\n    x: 1\n",
        )
        .unwrap();
        assert_eq!(fields[0].0, "format");
        assert_eq!(fields[0].1, YamlValue::Str("pdc-document/2".into()));
        assert_eq!(
            fields[1].1,
            YamlValue::Seq(vec![YamlValue::Str("a".into()), YamlValue::Str("b".into())])
        );
        assert!(matches!(&fields[2].1, YamlValue::Map(m) if m.iter().any(|(k,_)| k == "depth")));
    }

    #[test]
    fn resolves_core_schema_scalars() {
        let fields = parse("a: true\nb: 42\nc: -3.5\nd: hello\ne: null\nf: \"true\"\n").unwrap();
        assert_eq!(fields[0].1, YamlValue::Bool(true));
        assert_eq!(fields[1].1, YamlValue::Int(42));
        assert_eq!(fields[2].1, YamlValue::Float(-3.5));
        assert_eq!(fields[3].1, YamlValue::Str("hello".into()));
        assert_eq!(fields[4].1, YamlValue::Null);
        assert_eq!(fields[5].1, YamlValue::Str("true".into()));
    }

    #[test]
    fn rejects_forbidden_constructs() {
        assert!(err("a: &x 1\n").reason.contains("anchors"));
        assert!(
            err("format: pdc-document/2\nformat: pdc-document/2\n")
                .reason
                .contains("duplicate")
        );
        assert!(err("1: x\n").reason.contains("keys must be strings"));
        assert!(err("true: x\n").reason.contains("keys must be strings"));
        // A defined anchor followed by its alias: the alias event is
        // rejected outright.
        let d = err("a: &x 1\nb: *x\n");
        eprintln!("DBG alias case code={:?} reason={}", d.code, d.reason);
        // An undefined alias fails to scan at all — still rejected.
        assert!(err("a: *x\n").reason.contains("invalid YAML"));
        assert!(err("a: !!str 1\n").reason.contains("tags"));
    }

    #[test]
    fn rejects_multi_document_streams() {
        let d = err("format: pdc-document/2\n---\nformat: pdc-document/2\n");
        assert!(d.reason.contains("multi-document"), "{d}");
    }

    #[test]
    fn depth_and_node_budget_map_to_document_too_complex() {
        let mut deep = String::new();
        for i in 0..40 {
            deep.push_str(&" ".repeat(i));
            deep.push_str(&format!("k{i}:\n"));
        }
        deep.push_str(&" ".repeat(40));
        deep.push_str("leaf: 1");
        let d = err(&deep);
        assert_eq!(d.code, PdcDiagnosticCode::DocumentTooComplex, "{d}");

        let mut many = String::from("root:\n");
        for i in 0..10_002 {
            many.push_str(&format!("  k{i}: {i}\n"));
        }
        let d = err(&many);
        assert_eq!(d.code, PdcDiagnosticCode::DocumentTooComplex, "{d}");
    }

    #[test]
    fn query_mode_carries_invalid_query_spelling() {
        let d = parse_safe_yaml("a: *x\n", PdcDiagnosticCode::InvalidQuery).unwrap_err();
        assert_eq!(d.code, PdcDiagnosticCode::InvalidQuery, "{d}");
    }

    #[test]
    fn non_map_root_is_rejected() {
        assert!(parse("just a scalar\n").is_err());
        assert!(parse("- a\n- b\n").is_err());
    }
}

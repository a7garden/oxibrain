//! PDC constrained-envelope parser and standard-field validation (§5).
//!
//! The envelope grammar is frozen constrained YAML, not general YAML:
//!
//! - keys are nonempty, case-sensitive, at indentation level zero;
//! - a value is a Boolean, single-line string, flat string sequence (flow or
//!   block), literal block string, or exactly one nested map with two-space
//!   children;
//! - duplicate keys, tabs, comments, anchors, aliases, tags, complex keys,
//!   multi-document streams, and empty values are forbidden;
//! - `true`/`false` are Booleans; every other scalar is a string (no implicit
//!   numbers or dates).
//!
//! The parser is deliberately hand-rolled: general YAML libraries are allowed
//! only behind validation that rejects every forbidden feature, and the
//! constrained grammar is small enough that rejecting by construction is
//! simpler and safer than rejecting after the fact.

use super::BodyProfile;
use super::diagnostic::{PdcDiagnostic, PdcDiagnosticCode};

/// One envelope value in observed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvValue {
    Bool(bool),
    Str(String),
    /// Flow `[a, b]` or block sequence of flat strings.
    Seq(Vec<String>),
    /// Literal block string (`|`, `|-`, `|+`), chomped per the marker.
    Block(String),
    /// Exactly one nested map; children are scalar strings (Booleans keep
    /// their spelling), preserved opaquely for diagnostics.
    Map(Vec<(String, String)>),
}

/// The raw envelope: fields in observed order. Unknown fields and extension
/// maps are preserved here so diagnostics can name them; oxibrain never
/// re-serializes an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawEnvelope {
    pub fields: Vec<(String, EnvValue)>,
}

impl RawEnvelope {
    pub fn get(&self, key: &str) -> Option<&EnvValue> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Envelope grammar violation with a 1-based line inside the envelope slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeError {
    pub line: usize,
    pub message: String,
}

impl EnvelopeError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

/// Parse the envelope source (the text between the transport markers, with
/// CRLF already normalized by the transport layer).
pub fn parse_envelope(src: &str) -> Result<RawEnvelope, EnvelopeError> {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out = RawEnvelope::default();
    let mut i = 0;
    let total = lines.len();

    while i < total {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(EnvelopeError::new(
                i + 1,
                "unexpected indentation at envelope top level (keys sit at column zero)",
            ));
        }
        let (key, rest) = split_key(line, i + 1)?;
        if out.fields.iter().any(|(k, _)| k == &key) {
            return Err(EnvelopeError::new(i + 1, format!("duplicate key `{key}`")));
        }
        let rest = rest.trim_start();
        if rest.is_empty() {
            // Empty value: either a nested map, a block sequence, or forbidden.
            let (value, next) = parse_children(&lines, i + 1, &key)?;
            out.fields.push((key, value));
            i = next;
            continue;
        }
        let (value, consumed_extra) = parse_inline_value(rest, &lines, i + 1)?;
        out.fields.push((key, value));
        i += 1 + consumed_extra;
    }
    Ok(out)
}

/// Observed top-level `format` value from raw envelope text, used to choose
/// the envelope grammar before parsing it (v1 constrained YAML vs. v2 safe
/// general YAML). Scans for a `format:` key at column zero and strips one
/// layer of matching quotes.
pub(super) fn observed_format(src: &str) -> Option<String> {
    for line in src.lines() {
        let Some(rest) = line.strip_prefix("format:") else {
            continue;
        };
        let value = rest.trim();
        let unquoted = match (value.chars().next(), value.chars().last()) {
            (Some(q), Some(last)) if (q == '"' || q == '\'') && q == last && value.len() >= 2 => {
                &value[1..value.len() - 1]
            }
            _ => value,
        };
        return Some(unquoted.to_string());
    }
    None
}

/// Split `key: rest`. The colon must terminate the key (no quoted keys, no
/// complex keys). Forbidden characters in keys or lines fail with `reason`.
fn split_key(line: &str, line_no: usize) -> Result<(String, &str), EnvelopeError> {
    if line.contains('\t') {
        return Err(EnvelopeError::new(
            line_no,
            "tabs are forbidden in the envelope",
        ));
    }
    if let Some(rest) = line.strip_prefix("? ") {
        let _ = rest;
        return Err(EnvelopeError::new(line_no, "complex keys are forbidden"));
    }
    let colon = line.find(':').ok_or_else(|| {
        EnvelopeError::new(line_no, format!("expected `key: value`, got `{line}`"))
    })?;
    let key = &line[..colon];
    if key.is_empty() {
        return Err(EnvelopeError::new(line_no, "empty keys are forbidden"));
    }
    if key.starts_with(['&', '*', '!']) {
        return Err(EnvelopeError::new(
            line_no,
            "anchors, aliases, and tags are forbidden in the envelope",
        ));
    }
    if key.starts_with(['{', '[']) {
        return Err(EnvelopeError::new(
            line_no,
            "complex (flow) keys are forbidden",
        ));
    }
    Ok((key.trim_end().to_string(), &line[colon + 1..]))
}

/// Parse children after `key:` with an empty inline value: either a nested map
/// (`  child: value`) or a block sequence (`  - item`), both at exactly two
/// spaces. Deeper indentation is forbidden. Returns the value and the index of
/// the first line NOT consumed.
fn parse_children(
    lines: &[&str],
    start: usize,
    parent: &str,
) -> Result<(EnvValue, usize), EnvelopeError> {
    let mut i = start;
    // Skip blank lines before deciding.
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i >= lines.len() {
        return Err(EnvelopeError::new(
            start,
            format!("empty value for key `{parent}`"),
        ));
    }
    let first = lines[i];
    if first.starts_with('\t') {
        return Err(EnvelopeError::new(
            i + 1,
            "tabs are forbidden in the envelope",
        ));
    }
    if !first.starts_with("  ") {
        return Err(EnvelopeError::new(
            start,
            format!("empty value for key `{parent}` (children, when present, indent two spaces)"),
        ));
    }
    if first[2..].starts_with("- ") || first[2..].starts_with('-') {
        // Block sequence at exactly two spaces.
        let mut items = Vec::new();
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                i += 1;
                continue;
            }
            let Some(item) = line.strip_prefix("  - ") else {
                if line == "  -" {
                    return Err(EnvelopeError::new(
                        i + 1,
                        "empty sequence items are forbidden",
                    ));
                }
                break;
            };
            if line.starts_with("   ") && !line.starts_with("  - ") {
                return Err(EnvelopeError::new(i + 1, "deeper indentation is forbidden"));
            }
            items.push(plain_scalar(item, i + 1)?);
            i += 1;
        }
        if items.is_empty() {
            return Err(EnvelopeError::new(
                start,
                format!("empty value for key `{parent}`"),
            ));
        }
        return Ok((EnvValue::Seq(items), i));
    }
    // Nested map: children at exactly two spaces, scalar values only.
    let mut map = Vec::new();
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if !line.starts_with("  ") {
            break;
        }
        if line.starts_with("   ") {
            return Err(EnvelopeError::new(
                i + 1,
                "maps deeper than one level are forbidden",
            ));
        }
        let child = &line[2..];
        if child.starts_with("- ") {
            return Err(EnvelopeError::new(i + 1, "sequences of maps are forbidden"));
        }
        let (ckey, crest) = split_key(child, i + 1)?;
        let cval = crest.trim();
        if cval.is_empty() {
            return Err(EnvelopeError::new(
                i + 1,
                format!("empty value for `{ckey}` inside map `{parent}`"),
            ));
        }
        // Children are plain scalars; flow/block constructs are forbidden here.
        if cval.starts_with(['[', '{', '|', '>']) {
            return Err(EnvelopeError::new(
                i + 1,
                format!("value of `{ckey}` inside map `{parent}` must be a plain scalar"),
            ));
        }
        map.push((ckey, plain_scalar(cval, i + 1)?));
        i += 1;
    }
    if map.is_empty() {
        return Err(EnvelopeError::new(
            start,
            format!("empty value for key `{parent}`"),
        ));
    }
    Ok((EnvValue::Map(map), i))
}

/// Parse an inline value (after `key: `). Literal blocks consume following
/// lines and return the number of extra lines consumed.
fn parse_inline_value(
    rest: &str,
    lines: &[&str],
    line_no: usize,
) -> Result<(EnvValue, usize), EnvelopeError> {
    if rest.starts_with('|') {
        let marker = rest;
        let chomp = match marker {
            "|" => Chomp::Clip,
            "|-" => Chomp::Strip,
            "|+" => Chomp::Keep,
            other => {
                return Err(EnvelopeError::new(
                    line_no,
                    format!("unsupported block scalar marker `{other}`"),
                ));
            }
        };
        let (block, consumed) = parse_block_scalar(lines, line_no, chomp)?;
        return Ok((EnvValue::Block(block), consumed));
    }
    if rest.starts_with('>') {
        return Err(EnvelopeError::new(
            line_no,
            "folded block scalars are not part of the envelope grammar",
        ));
    }
    if rest.starts_with('[') {
        let seq = parse_flow_sequence(rest, line_no)?;
        return Ok((EnvValue::Seq(seq), 0));
    }
    if rest.starts_with('{') {
        return Err(EnvelopeError::new(
            line_no,
            "flow mappings are forbidden in the envelope",
        ));
    }
    // Bare unquoted `true`/`false` are Booleans; quoted spellings stay strings.
    let scalar = plain_scalar(rest, line_no)?;
    if !rest.starts_with(['\'', '"']) {
        match scalar.as_str() {
            "true" => return Ok((EnvValue::Bool(true), 0)),
            "false" => return Ok((EnvValue::Bool(false), 0)),
            _ => {}
        }
    }
    Ok((EnvValue::Str(scalar), 0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chomp {
    Clip,
    Strip,
    Keep,
}

/// Literal block scalar: following lines indented at least two spaces belong
/// to the block; the content indent is set by the first non-empty line and
/// every later line must not indent less than it. Blank lines are allowed and
/// never end the block.
fn parse_block_scalar(
    lines: &[&str],
    line_no: usize,
    chomp: Chomp,
) -> Result<(String, usize), EnvelopeError> {
    let mut i = line_no; // lines is 0-based; line_no is the 1-based index of the NEXT line
    let mut content: Vec<String> = Vec::new();
    let mut indent: Option<usize> = None;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            content.push(String::new());
            i += 1;
            continue;
        }
        if line.contains('\t') && line.trim_start().is_empty() {
            return Err(EnvelopeError::new(
                i + 1,
                "tabs are forbidden in the envelope",
            ));
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        if spaces < 2 {
            break; // dedent: block ends
        }
        let want = *indent.get_or_insert(spaces);
        if spaces < want {
            break;
        }
        content.push(line[want..].to_string());
        i += 1;
    }
    // Trim trailing blank lines per chomping.
    while content.last().is_some_and(|l| l.is_empty()) {
        content.pop();
        if chomp == Chomp::Strip {
            // Strip: nothing re-added.
        } else {
            // Clip/Keep re-add exactly the newlines below.
        }
    }
    let mut out = content.join("\n");
    match chomp {
        Chomp::Strip => {}
        Chomp::Clip | Chomp::Keep => {
            if !out.is_empty() {
                out.push('\n');
            }
        }
    }
    if chomp == Chomp::Keep {
        // Keep: preserve the original count of trailing blank lines.
        let mut j = i;
        while j > line_no && lines[j - 1].trim().is_empty() {
            out.push('\n');
            j -= 1;
        }
    }
    Ok((out, i - line_no))
}

/// `[a, b, "c d"]` — single-line flat string sequence. `[]` is an empty
/// sequence. Empty items, trailing commas, and multi-line sequences fail.
fn parse_flow_sequence(rest: &str, line_no: usize) -> Result<Vec<String>, EnvelopeError> {
    let close = rest
        .rfind(']')
        .ok_or_else(|| EnvelopeError::new(line_no, "flow sequence must close on the same line"))?;
    let inner = &rest[1..close];
    if !rest[close + 1..].trim().is_empty() {
        return Err(EnvelopeError::new(
            line_no,
            "unexpected text after flow sequence close",
        ));
    }
    let mut items = Vec::new();
    if inner.trim().is_empty() {
        return Ok(items);
    }
    for raw in split_flow_items(inner, line_no)? {
        let item = raw.trim();
        if item.is_empty() {
            return Err(EnvelopeError::new(
                line_no,
                "empty flow sequence items are forbidden",
            ));
        }
        items.push(plain_scalar(item, line_no)?);
    }
    Ok(items)
}

/// Split a flow sequence body on top-level commas (bracket depth zero,
/// outside quotes). Strings may be quoted with `'` or `"`; escapes are not
/// part of the grammar.
fn split_flow_items(inner: &str, line_no: usize) -> Result<Vec<String>, EnvelopeError> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let bytes = inner.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let c = inner[idx..].chars().next().expect("nonempty");
        let c_len = c.len_utf8();
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    items.push(inner[start..idx].to_string());
                    start = idx + 1;
                }
                _ => {}
            },
        }
        idx += c_len;
    }
    if quote.is_some() {
        return Err(EnvelopeError::new(
            line_no,
            "unterminated quoted string in flow sequence",
        ));
    }
    if depth != 0 {
        return Err(EnvelopeError::new(
            line_no,
            "nested flow collections are forbidden",
        ));
    }
    items.push(inner[start..].to_string());
    Ok(items)
}

/// Plain scalar with the grammar's quoting and comment rules:
/// - `true`/`false` are Booleans;
/// - a leading quote must terminate on the same line; everything after it is
///   forbidden;
/// - an unquoted ` #` starts a comment — comments are forbidden outright;
/// - anchors/aliases/tags are forbidden.
fn plain_scalar(raw: &str, line_no: usize) -> Result<String, EnvelopeError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(EnvelopeError::new(line_no, "empty values are forbidden"));
    }
    if s.starts_with(['&', '*', '!']) {
        return Err(EnvelopeError::new(
            line_no,
            "anchors, aliases, and tags are forbidden in the envelope",
        ));
    }
    if let Some(q) = s.chars().next().filter(|c| *c == '\'' || *c == '"') {
        let body = &s[1..];
        let end = body.find(q).ok_or_else(|| {
            EnvelopeError::new(
                line_no,
                "unterminated quoted scalar (must close on the same line)",
            )
        })?;
        if !body[end + 1..].trim().is_empty() {
            return Err(EnvelopeError::new(
                line_no,
                "unexpected text after quoted scalar",
            ));
        }
        return Ok(body[..end].to_string());
    }
    if let Some(hash) = s.find(" #") {
        let _ = hash;
        return Err(EnvelopeError::new(
            line_no,
            "comments are forbidden in the envelope",
        ));
    }
    if s == "#" {
        return Err(EnvelopeError::new(
            line_no,
            "comments are forbidden in the envelope",
        ));
    }
    Ok(s.to_string())
}

// --- standard field validation (§5.1–§5.2) ----------------------------------

/// Parsed and validated standard fields for one profile.
pub(super) fn validate_fields(
    raw: &RawEnvelope,
    transport_profile: BodyProfile,
) -> Result<super::PdcMetadata, PdcDiagnostic> {
    use PdcDiagnosticCode as Code;

    let invalid = |msg: String| PdcDiagnostic::new(Code::InvalidEnvelope, msg);

    // format: exact value.
    let format_ok = matches!(raw.get("format"), Some(EnvValue::Str(s)) if s == "pdc-document/1");
    if !format_ok {
        let observed = match raw.get("format") {
            Some(EnvValue::Str(s)) => s.clone(),
            _ => String::new(),
        };
        if observed.starts_with("pdc-document/") {
            return Err(PdcDiagnostic::new(
                Code::UnsupportedDocumentVersion,
                format!("unsupported document version `{observed}`"),
            ));
        }
        return Err(invalid(format!(
            "`format` must be exactly `pdc-document/1` (observed `{observed}`)"
        )));
    }

    // body: exact profile matching the transport; same-profile wrong version
    // or unknown values are unsupported_body_version.
    let body = match raw.get("body") {
        Some(EnvValue::Str(s)) => match BodyProfile::from_str_exact(s) {
            Some(p) if p == transport_profile => p,
            Some(_) => {
                return Err(PdcDiagnostic::new(
                    Code::InvalidTransport,
                    format!(
                        "envelope declares `{s}` but the file transport is `{}`",
                        transport_profile.as_str()
                    ),
                ));
            }
            None => {
                return Err(PdcDiagnostic::new(
                    Code::UnsupportedBodyVersion,
                    format!("unsupported body profile `{s}`"),
                ));
            }
        },
        _ => {
            return Err(invalid(
                "`body` is required and must be a string".to_string(),
            ));
        }
    };

    // id: canonical lowercase hyphenated UUID.
    let id = match raw.get("id") {
        Some(EnvValue::Str(s)) => s,
        _ => return Err(invalid("`id` is required and must be a string".to_string())),
    };
    if !is_canonical_uuid(id) {
        return Err(PdcDiagnostic::new(
            Code::InvalidDocumentId,
            format!("`id` must be a canonical lowercase hyphenated UUID (observed `{id}`)"),
        ));
    }

    // created/updated: canonical timestamps, updated >= created.
    let created = string_field(raw, "created")?;
    let updated = string_field(raw, "updated")?;
    validate_canonical_timestamp(&created).map_err(invalid_containing("created"))?;
    validate_canonical_timestamp(&updated).map_err(invalid_containing("updated"))?;
    if updated.as_str() < created.as_str() {
        return Err(invalid(
            "`updated` must not be earlier than `created`".to_string(),
        ));
    }

    // title: any string, may be empty.
    let title = match raw.get("title") {
        Some(EnvValue::Str(s)) => s.clone(),
        _ => {
            return Err(invalid(
                "`title` is required and must be a string".to_string(),
            ));
        }
    };

    // Optional standard fields.
    let profile = opt_string_field(raw, "profile")?;
    let lang = opt_string_field(raw, "lang")?;
    let tags = opt_seq_field(raw, "tags")?;
    let aliases = opt_seq_field(raw, "aliases")?;
    let favorite = match raw.get("favorite") {
        None => false,
        Some(EnvValue::Bool(b)) => *b,
        Some(_) => return Err(invalid("`favorite` must be a Boolean".to_string())),
    };
    let deleted = match raw.get("deleted") {
        None => false,
        Some(EnvValue::Bool(b)) => *b,
        Some(_) => return Err(invalid("`deleted` must be a Boolean".to_string())),
    };
    let deleted_at = match raw.get("deleted_at") {
        None => None,
        Some(EnvValue::Str(s)) => {
            validate_canonical_timestamp(s).map_err(invalid_containing("deleted_at"))?;
            Some(s.clone())
        }
        Some(_) => return Err(invalid("`deleted_at` must be a string".to_string())),
    };
    if deleted != deleted_at.is_some() {
        return Err(invalid(
            "`deleted_at` must be present exactly when `deleted` is true".to_string(),
        ));
    }

    // Tags/aliases are case-sensitive and duplicate-free.
    for (field, seq) in [("tags", &tags), ("aliases", &aliases)] {
        let mut seen = std::collections::BTreeSet::new();
        for item in seq {
            if !seen.insert(item.as_str()) {
                return Err(invalid(format!("duplicate {field} label `{item}`")));
            }
        }
    }

    Ok(super::PdcMetadata {
        contract_version: 1,
        document_uuid: id.clone(),
        body,
        created,
        updated,
        title,
        profile,
        lang,
        tags,
        aliases,
        cssclasses: Vec::new(),
        favorite,
        deleted,
        deleted_at,
    })
}
// --- pdc-document/2 standard field validation (PDC-2.0 §5) -------------------

use super::yaml_frontmatter::YamlValue;

/// Validate the standard fields of a `pdc-document/2` envelope (safe general
/// YAML already parsed by [`super::yaml_frontmatter::parse_safe_yaml`]).
/// Unknown top-level keys are user properties (§5.3): they are legal here and
/// simply not projected.
pub(super) fn validate_fields_v2(
    fields: &[(String, YamlValue)],
    transport_profile: BodyProfile,
) -> Result<super::PdcMetadata, PdcDiagnostic> {
    use PdcDiagnosticCode as Code;

    let invalid = |msg: String| PdcDiagnostic::new(Code::InvalidEnvelope, msg);
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);

    // format: exact value; an unknown pdc-document major is unsupported, not
    // malformed (§2).
    let format_ok = matches!(get("format"), Some(YamlValue::Str(s)) if s == "pdc-document/2");
    if !format_ok {
        let observed = match get("format") {
            Some(YamlValue::Str(s)) => s.clone(),
            _ => String::new(),
        };
        if observed.starts_with("pdc-document/") {
            return Err(PdcDiagnostic::new(
                Code::UnsupportedDocumentVersion,
                format!("unsupported document version `{observed}`"),
            ));
        }
        return Err(invalid(format!(
            "`format` must be exactly `pdc-document/2` (observed `{observed}`)"
        )));
    }

    // body: exact profile matching the transport. `pdc-djot/1` is valid only
    // inside a pdc-document/1 envelope (§2), so under v2 it is a transport
    // mismatch; other unknown profiles are unsupported versions.
    let body = match get("body") {
        Some(YamlValue::Str(s)) => match BodyProfile::from_str_exact(s) {
            Some(p) if p == transport_profile => p,
            Some(_) | None if s == "pdc-djot/1" => {
                return Err(PdcDiagnostic::new(
                    Code::InvalidTransport,
                    "`pdc-djot/1` is valid only inside a `pdc-document/1` envelope (PDC 2 §2)"
                        .to_string(),
                ));
            }
            Some(_) => {
                return Err(PdcDiagnostic::new(
                    Code::InvalidTransport,
                    format!(
                        "envelope declares `{s}` but the file transport is `{}`",
                        transport_profile.as_str()
                    ),
                ));
            }
            None => {
                return Err(PdcDiagnostic::new(
                    Code::UnsupportedBodyVersion,
                    format!("unsupported body profile `{s}`"),
                ));
            }
        },
        _ => {
            return Err(invalid(
                "`body` is required and must be a string".to_string(),
            ));
        }
    };

    // id: canonical lowercase hyphenated UUID (§7.1).
    let id = match get("id") {
        Some(YamlValue::Str(s)) => s.clone(),
        _ => return Err(invalid("`id` is required and must be a string".to_string())),
    };
    if !is_canonical_uuid(&id) {
        return Err(PdcDiagnostic::new(
            Code::InvalidDocumentId,
            format!("`id` must be a canonical lowercase hyphenated UUID (observed `{id}`)"),
        ));
    }

    // created/updated: canonical timestamps, updated >= created (§5.1).
    let created = v2_string_field(fields, "created")?;
    let updated = v2_string_field(fields, "updated")?;
    validate_canonical_timestamp(&created).map_err(invalid_containing("created"))?;
    validate_canonical_timestamp(&updated).map_err(invalid_containing("updated"))?;
    if updated.as_str() < created.as_str() {
        return Err(invalid(
            "`updated` must not be earlier than `created`".to_string(),
        ));
    }

    // title: any string, may be empty.
    let title = v2_string_field(fields, "title")?;

    // Optional standard fields (§5.2); strict types, strict value shapes.
    let profile = v2_opt_string_field(fields, "profile")?;
    let lang = v2_opt_string_field(fields, "lang")?;
    let tags = v2_seq_field(fields, "tags")?;
    let aliases = v2_seq_field(fields, "aliases")?;
    let cssclasses = v2_seq_field(fields, "cssclasses")?;
    let favorite = v2_bool_field(fields, "favorite")?.unwrap_or(false);
    let deleted = v2_bool_field(fields, "deleted")?.unwrap_or(false);
    let deleted_at = match get("deleted_at") {
        None => None,
        Some(YamlValue::Str(s)) => {
            validate_canonical_timestamp(s).map_err(invalid_containing("deleted_at"))?;
            Some(s.clone())
        }
        Some(_) => return Err(invalid("`deleted_at` must be a string".to_string())),
    };
    if deleted != deleted_at.is_some() {
        return Err(invalid(
            "`deleted_at` must be present exactly when `deleted` is true".to_string(),
        ));
    }

    // Tags/aliases/cssclasses are case-sensitive and duplicate-free.
    for (field, seq) in [
        ("tags", &tags),
        ("aliases", &aliases),
        ("cssclasses", &cssclasses),
    ] {
        let mut seen = std::collections::BTreeSet::new();
        for item in seq {
            if !seen.insert(item.as_str()) {
                return Err(invalid(format!("duplicate {field} label `{item}`")));
            }
        }
    }

    Ok(super::PdcMetadata {
        contract_version: 2,
        document_uuid: id,
        body,
        created,
        updated,
        title,
        profile,
        lang,
        tags,
        aliases,
        cssclasses,
        favorite,
        deleted,
        deleted_at,
    })
}

fn v2_string_field(fields: &[(String, YamlValue)], key: &str) -> Result<String, PdcDiagnostic> {
    match fields.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
        Some(YamlValue::Str(s)) => Ok(s.clone()),
        _ => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` is required and must be a string"),
        )),
    }
}

fn v2_opt_string_field(
    fields: &[(String, YamlValue)],
    key: &str,
) -> Result<Option<String>, PdcDiagnostic> {
    match fields.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
        None => Ok(None),
        Some(YamlValue::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` must be a string"),
        )),
    }
}

fn v2_seq_field(fields: &[(String, YamlValue)], key: &str) -> Result<Vec<String>, PdcDiagnostic> {
    match fields.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
        None => Ok(Vec::new()),
        Some(YamlValue::Seq(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    YamlValue::Str(s) => out.push(s.clone()),
                    _ => {
                        return Err(PdcDiagnostic::new(
                            PdcDiagnosticCode::InvalidEnvelope,
                            format!("`{key}` must be a string sequence"),
                        ));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` must be a string sequence"),
        )),
    }
}

fn v2_bool_field(fields: &[(String, YamlValue)], key: &str) -> Result<Option<bool>, PdcDiagnostic> {
    match fields.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
        None => Ok(None),
        Some(YamlValue::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` must be a Boolean"),
        )),
    }
}

fn invalid_containing(field: &'static str) -> impl Fn(String) -> PdcDiagnostic {
    move |msg: String| {
        PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("{field}: {msg}"),
        )
    }
}

fn string_field(raw: &RawEnvelope, key: &str) -> Result<String, PdcDiagnostic> {
    match raw.get(key) {
        Some(EnvValue::Str(s)) => Ok(s.clone()),
        _ => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` is required and must be a string"),
        )),
    }
}

fn opt_string_field(raw: &RawEnvelope, key: &str) -> Result<Option<String>, PdcDiagnostic> {
    match raw.get(key) {
        None => Ok(None),
        Some(EnvValue::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` must be a string"),
        )),
    }
}

fn opt_seq_field(raw: &RawEnvelope, key: &str) -> Result<Vec<String>, PdcDiagnostic> {
    match raw.get(key) {
        None => Ok(Vec::new()),
        Some(EnvValue::Seq(items)) => Ok(items.clone()),
        Some(_) => Err(PdcDiagnostic::new(
            PdcDiagnosticCode::InvalidEnvelope,
            format!("`{key}` must be a flat string sequence"),
        )),
    }
}

/// Canonical lowercase hyphenated UUID: 8-4-4-4-12 lowercase hex.
pub(super) fn is_canonical_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            _ => {
                if !matches!(b, b'0'..=b'9' | b'a'..=b'f') {
                    return false;
                }
            }
        }
    }
    true
}

/// Canonical UTC timestamp `YYYY-MM-DDTHH:MM:SS.sssZ` with exactly millisecond
/// precision and a real Gregorian calendar date and time.
pub(super) fn validate_canonical_timestamp(s: &str) -> Result<(), String> {
    let bytes = s.as_bytes();
    if bytes.len() != 24 {
        return Err(format!("timestamp `{s}` is not `YYYY-MM-DDTHH:MM:SS.sssZ`"));
    }
    let digit = |i: usize| (bytes[i] as char).to_digit(10);
    let fixed = |i: usize, c: u8| bytes[i] == c;
    let ok_shape = fixed(4, b'-')
        && fixed(7, b'-')
        && fixed(10, b'T')
        && fixed(13, b':')
        && fixed(16, b':')
        && fixed(19, b'.')
        && fixed(23, b'Z')
        && (0..24).all(|i| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 23) || digit(i).is_some());
    if !ok_shape {
        return Err(format!("timestamp `{s}` is not `YYYY-MM-DDTHH:MM:SS.sssZ`"));
    }
    let num = |from: usize, len: usize| -> u32 {
        s[from..from + len]
            .parse()
            .expect("digits validated by shape check")
    };
    let (year, month, day) = (num(0, 4), num(5, 2), num(8, 2));
    let (hour, min, sec) = (num(11, 2), num(14, 2), num(17, 2));
    if !(1..=12).contains(&month) {
        return Err(format!("timestamp `{s}` has month {month}"));
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => unreachable!("month range checked"),
    };
    if !(1..=dim).contains(&day) {
        return Err(format!("timestamp `{s}` has day {day}"));
    }
    if hour > 23 {
        return Err(format!("timestamp `{s}` has hour {hour}"));
    }
    if min > 59 {
        return Err(format!("timestamp `{s}` has minute {min}"));
    }
    if sec > 59 {
        return Err(format!("timestamp `{s}` has second {sec}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_envelope() {
        let src = "format: pdc-document/1\nbody: pdc-djot/1\nid: 018f47c6-4a77-7c52-9db8-0e5f9bcb17db\ncreated: 2026-09-13T12:34:56.789Z\nupdated: 2026-09-13T12:34:56.789Z\ntitle: Minimal document\n";
        let raw = parse_envelope(src).unwrap();
        let meta = validate_fields(&raw, BodyProfile::Djot).unwrap();
        assert_eq!(meta.title, "Minimal document");
        assert!(!meta.favorite);
        assert_eq!(meta.tags, Vec::<String>::new());
    }

    #[test]
    fn duplicate_key_is_invalid_envelope() {
        let src = "format: pdc-document/1\nformat: pdc-document/1\n";
        let err = parse_envelope(src).unwrap_err();
        assert!(err.message.contains("duplicate key"));
    }

    #[test]
    fn comment_in_plain_scalar_is_forbidden() {
        let src = "title: Comments are forbidden # general YAML is not the envelope grammar\n";
        let err = parse_envelope(src).unwrap_err();
        assert!(err.message.contains("comments are forbidden"));
    }

    #[test]
    fn hash_inside_quoted_scalar_is_allowed() {
        let src = "title: \"issue #42\"\n";
        let raw = parse_envelope(src).unwrap();
        assert_eq!(
            raw.get("title"),
            Some(&EnvValue::Str("issue #42".to_string()))
        );
    }

    #[test]
    fn flow_and_block_sequences_agree() {
        let flow = parse_envelope("tags: [a, b]\n").unwrap();
        let block = parse_envelope("tags:\n  - a\n  - b\n").unwrap();
        assert_eq!(flow.get("tags"), block.get("tags"));
    }

    #[test]
    fn nested_map_children_preserved() {
        let raw = parse_envelope("x_sawhorse:\n  legacy_id: FDR-001\n").unwrap();
        assert_eq!(
            raw.get("x_sawhorse"),
            Some(&EnvValue::Map(vec![(
                "legacy_id".to_string(),
                "FDR-001".to_string()
            )]))
        );
    }

    #[test]
    fn calendar_validation_rejects_feb_30() {
        assert!(validate_canonical_timestamp("2026-02-30T12:34:56.789Z").is_err());
        assert!(validate_canonical_timestamp("2026-02-28T12:34:56.789Z").is_ok());
        assert!(validate_canonical_timestamp("2024-02-29T12:34:56.789Z").is_ok());
        assert!(validate_canonical_timestamp("2026-13-01T00:00:00.000Z").is_err());
        assert!(validate_canonical_timestamp("2026-09-13T24:00:00.000Z").is_err());
        assert!(validate_canonical_timestamp("2026-09-13 12:34:56.789Z").is_err());
    }

    #[test]
    fn updated_before_created_is_invalid() {
        let src = "format: pdc-document/1\nbody: pdc-djot/1\nid: 018f47c7-0359-7975-89ec-6684f0fcc14c\ncreated: 2026-09-13T13:00:00.000Z\nupdated: 2026-09-13T12:00:00.000Z\ntitle: t\n";
        let raw = parse_envelope(src).unwrap();
        let err = validate_fields(&raw, BodyProfile::Djot).unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::InvalidEnvelope);
    }

    #[test]
    fn deleted_at_pairing_is_enforced() {
        let base = "format: pdc-document/1\nbody: pdc-djot/1\nid: 018f47c7-0f16-77d0-8b6a-e86481850617\ncreated: 2026-09-13T12:34:56.789Z\nupdated: 2026-09-13T12:34:56.789Z\ntitle: t\ndeleted: true\n";
        let raw = parse_envelope(base).unwrap();
        let err = validate_fields(&raw, BodyProfile::Djot).unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::InvalidEnvelope);
        let ok = parse_envelope(&format!("{base}deleted_at: 2026-09-13T12:34:56.789Z\n")).unwrap();
        assert!(validate_fields(&ok, BodyProfile::Djot).is_ok());
    }

    #[test]
    fn uuid_case_is_enforced() {
        assert!(is_canonical_uuid("018f47c6-4a77-7c52-9db8-0e5f9bcb17db"));
        assert!(!is_canonical_uuid("018F47C6-8AEA-7F30-A70F-1ED00DF4CC25"));
        assert!(!is_canonical_uuid("018f47c64a777c529db80e5f9bcb17db"));
    }

    #[test]
    fn body_version_and_transport_mismatch_codes() {
        let future = "format: pdc-document/1\nbody: pdc-djot/2\nid: 018f47c6-8aea-7f30-a70f-1ed00df4cc25\ncreated: 2026-09-13T12:34:56.789Z\nupdated: 2026-09-13T12:34:56.789Z\ntitle: t\n";
        let raw = parse_envelope(future).unwrap();
        let err = validate_fields(&raw, BodyProfile::Djot).unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::UnsupportedBodyVersion);

        let mismatch = "format: pdc-document/1\nbody: pdc-html/1\nid: 018f47c6-8aea-7f30-a70f-1ed00df4cc25\ncreated: 2026-09-13T12:34:56.789Z\nupdated: 2026-09-13T12:34:56.789Z\ntitle: t\n";
        let raw = parse_envelope(mismatch).unwrap();
        let err = validate_fields(&raw, BodyProfile::Djot).unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::InvalidTransport);
    }
}

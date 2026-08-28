//! Space identity rules. Pure decisions only (P9).

use oxibrain_ports::BrainError;

/// A space name is an identifier: it becomes a directory name under
/// `~/.oxi/vault/` and a token in config. Letters (any script), digits,
/// `-`, `_`; length 1..=64 after trimming. Spec §4.2.
pub fn validate_space_name(raw: &str) -> Result<String, BrainError> {
    let name = raw.trim();
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: "empty after trimming".into(),
        });
    }
    if chars.len() > 64 {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: format!("{} chars (max 64)", chars.len()),
        });
    }
    if let Some(bad) = chars
        .iter()
        .find(|c| !c.is_alphanumeric() && **c != '-' && **c != '_')
    {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: format!("disallowed character {bad:?} (letters, digits, '-', '_')"),
        });
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_slug_and_unicode() {
        assert_eq!(validate_space_name("personal").unwrap(), "personal");
        assert_eq!(validate_space_name(" dev-2 ").unwrap(), "dev-2");
        assert_eq!(validate_space_name("개인").unwrap(), "개인");
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_space_name("").is_err());
        assert!(validate_space_name("   ").is_err());
        assert!(validate_space_name("has space").is_err());
        assert!(validate_space_name("a/b").is_err());
        assert!(validate_space_name("a\\b").is_err());
        assert!(validate_space_name("a:b").is_err());
        assert!(validate_space_name(&"x".repeat(65)).is_err());
    }
}

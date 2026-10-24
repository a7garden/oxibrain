//! Narrow source-view lifecycle CLI. JSON comes only from bounded stdin;
//! credentials come from named environment handles, never the serialized body.
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::BrainError;
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use std::{
    fmt,
    io::{IsTerminal, Read, Write},
    path::Path,
};

pub const MAX_INPUT_BYTES: usize = 1_048_576;
const API: i64 = 1;
#[derive(Debug)]
struct InputError;
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Strict;
        impl<'de> Visitor<'de> for Strict {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one JSON value with unique object keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(json!(value)))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.into())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut values = vec![];
                while let Some(value) = access.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = access.next_entry::<String, UniqueValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Strict)
    }
}
fn input(reader: impl Read) -> Result<Value, InputError> {
    let mut bytes = vec![];
    reader
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| InputError)?;
    if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
        return Err(InputError);
    }
    let mut parser = serde_json::Deserializer::from_slice(&bytes);
    let value = UniqueValue::deserialize(&mut parser)
        .map_err(|_| InputError)?
        .0;
    parser.end().map_err(|_| InputError)?;
    if !value.is_object() {
        return Err(InputError);
    }
    Ok(value)
}
fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_uppercase() || (index > 0 && byte.is_ascii_digit())
        })
}
fn credential(
    name: Option<&str>,
    lookup: impl FnOnce(&str) -> Option<String>,
) -> Result<Option<String>, InputError> {
    name.map(|name| {
        if !valid_env_name(name) {
            return Err(InputError);
        }
        lookup(name)
            .filter(|value| {
                !value.is_empty() && value.len() <= 16_384 && !value.contains(['\r', '\n', '\0'])
            })
            .ok_or(InputError)
    })
    .transpose()
}
fn error(operation: &str, code: &str, summary: &str, retryable: bool, next: &str) -> Value {
    json!({"api":API,"ok":false,"op":format!("source.view.{operation}"),"status":"error","summary":summary,
        "error":{"code":code,"message":summary,"retryable":retryable,"retry_class":if operation=="read"{"same_reference"}else{"same_occurrence"}},
        "next_actions":[next],"artifacts":[],"stop_condition":"Do not create a replacement occurrence while the original result is unresolved."})
}
fn failure(operation: &str, problem: BrainError) -> (i32, Value) {
    let (exit, code, summary, retry, next) = match problem {
        BrainError::Unauthorized(_) | BrainError::Scope { .. } => (
            4,
            "publication_unavailable",
            "The current independent grants or source revision cannot authorize this operation.",
            false,
            "Check the intended credential handles and current source; stop if authority is absent.",
        ),
        BrainError::Invalid(_) | BrainError::SpaceNameInvalid { .. } => (
            2,
            "invalid_input",
            "The request does not match source-view-publication/1.",
            false,
            "Correct the versioned payload and retry the same source occurrence.",
        ),
        BrainError::NotFound(_) | BrainError::SpaceNotFound { .. } => (
            3,
            "not_found",
            "The requested source view is unavailable.",
            false,
            "Inspect the original source or receipt; stop if it is no longer accessible.",
        ),
        BrainError::Locked { .. } | BrainError::Busy(_) => (
            5,
            "locked",
            "The store is busy; the original occurrence remains the retry identity.",
            true,
            "Retry the unchanged request after the existing writer finishes.",
        ),
        BrainError::Conflict(_) => (
            6,
            "conflict",
            "The original source or operation identity conflicts with this request.",
            false,
            "Inspect the original receipt and current source before continuing.",
        ),
        BrainError::Budget(_) => (
            7,
            "budget",
            "The operation exceeds its bounded source-view budget.",
            false,
            "Reduce the selected view without changing an unresolved original occurrence.",
        ),
        _ => (
            9,
            "internal",
            "The source-view operation could not be confirmed.",
            false,
            "Inspect the original receipt and engine health before retrying the same occurrence.",
        ),
    };
    (exit, error(operation, code, summary, retry, next))
}
async fn execute(
    dir: &Path,
    operation: &str,
    payload: Value,
    origin: Option<String>,
    destination: Option<String>,
) -> (i32, Value) {
    let brain = match Brain::open(BrainConfig::at(dir)).await {
        Ok(brain) => brain,
        Err(error) => return failure(operation, error),
    };
    match brain
        .source_view_operation(operation, payload, origin, destination)
        .await
    {
        Ok(data) => {
            let artifacts = data
                .get("publication_id")
                .and_then(Value::as_str)
                .map(|id| vec![json!({"kind":"source_view","publication_id":id})])
                .unwrap_or_default();
            (
                0,
                json!({"api":API,"ok":true,"op":format!("source.view.{operation}"),"status":"success",
                "summary":"Source-view operation confirmed.","data":data,"next_actions":[],"artifacts":artifacts}),
            )
        }
        Err(error) => failure(operation, error),
    }
}
/// Prints one machine envelope. Error output never includes input bytes,
/// credential values, arbitrary engine diagnostics or private source metadata.
pub async fn run(
    dir: &Path,
    operation: &str,
    origin_env: Option<&str>,
    destination_env: Option<&str>,
) -> i32 {
    let parsed = if std::io::stdin().is_terminal() {
        Err(InputError)
    } else {
        input(std::io::stdin().lock())
    };
    let response = match parsed {
        Err(_) => (
            2,
            error(
                operation,
                "invalid_input",
                "Provide one JSON object on stdin, at most 1 MiB, with unique keys.",
                false,
                "Correct the bounded stdin payload before retrying the same occurrence.",
            ),
        ),
        Ok(payload) => {
            let origin = credential(origin_env, |name| std::env::var(name).ok());
            let destination = credential(destination_env, |name| std::env::var(name).ok());
            match (origin, destination) {
                (Ok(origin), Ok(destination)) => {
                    execute(dir, operation, payload, origin, destination).await
                }
                _ => (
                    4,
                    error(
                        operation,
                        "credential_unavailable",
                        "A required credential environment handle is invalid or unavailable.",
                        false,
                        "Set the intended environment variable; never put a credential in the payload or command arguments.",
                    ),
                ),
            }
        }
    };
    let Ok(mut bytes) = serde_json::to_vec(&response.1) else {
        return 9;
    };
    bytes.push(b'\n');
    if std::io::stdout().lock().write_all(&bytes).is_err() {
        return 9;
    }
    response.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_stdin_rejects_oversize_duplicate_nested_keys_trailing_data_and_non_objects() {
        for value in [
            b"[]".as_slice(),
            b"{\"a\":1,\"a\":2}",
            b"{\"a\":{\"key\":1,\"key\":2}}",
            b"{} {}",
            b"not-json",
        ] {
            assert!(input(value).is_err());
        }
        assert!(input(std::io::repeat(b' ').take((MAX_INPUT_BYTES + 1) as u64)).is_err());
        assert_eq!(
            input("{\"range\":{\"start\":0,\"end\":2},\"text\":\"한글\"}".as_bytes()).unwrap()["text"],
            "한글"
        );
    }
    #[test]
    fn credential_handles_never_accept_values_or_emit_secret_diagnostics() {
        for name in [
            "",
            "token-value",
            "lowercase",
            "9BAD",
            "A=B",
            "TOKEN\nOTHER",
        ] {
            assert!(credential(Some(name), |_| Some("secret".into())).is_err());
        }
        let secret = "credential-value-that-must-not-be-printed";
        assert_eq!(
            credential(Some("SOURCE_READ_TOKEN_1"), |_| Some(secret.into()))
                .unwrap()
                .as_deref(),
            Some(secret)
        );
        assert!(credential(Some("SOURCE_READ_TOKEN"), |_| None).is_err());
        let (_, envelope) = failure("publish", BrainError::Unauthorized(secret.into()));
        assert!(!envelope.to_string().contains(secret));
    }
}

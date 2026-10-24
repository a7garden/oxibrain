//! Native-only source-view operations. Authentication secrets live on the
//! session, outside method/model payloads, and the facade revalidates them.
use super::{BrainServer, INTERNAL_ERROR, INVALID_PARAMS, UNAUTHORIZED};
use serde_json::Value;
impl BrainServer {
    pub(super) async fn native_source_view(
        &self,
        method: &str,
        args: Option<&Value>,
    ) -> Result<Value, (i64, String)> {
        let payload = args
            .ok_or((INVALID_PARAMS, "missing source view request".into()))?
            .clone();
        let operation = method
            .strip_prefix("source_view.")
            .ok_or((INVALID_PARAMS, "invalid source view operation".into()))?;
        let origin = self.session_token.as_ref().map(|token| token.to_string());
        let destination = self
            .publication_destination_token
            .as_ref()
            .map(|token| token.to_string());
        self.brain
            .source_view_operation(operation, payload, origin, destination)
            .await
            .map_err(|error| match error {
                oxibrain::BrainError::Unauthorized(_) | oxibrain::BrainError::Scope { .. } => {
                    (UNAUTHORIZED, "publication unavailable".into())
                }
                oxibrain::BrainError::Invalid(_) | oxibrain::BrainError::Conflict(_) => (
                    INVALID_PARAMS,
                    "invalid publication request or conflicting occurrence".into(),
                ),
                _ => (INTERNAL_ERROR, "publication operation unavailable".into()),
            })
    }
}

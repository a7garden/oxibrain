//! Versioned CLI/native operation DTOs. Credentials are separate non-serialized
//! arguments; this surface does not add a model tool or accept a trust claim.
use crate::{Brain, PublicationCredentials};
use oxibrain_core::publication::{PublishViewV1, ViewOrigin, unavailable};
use oxibrain_ports::BrainError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
enum Version {
    #[serde(rename = "source-view-publication/1")]
    V1,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publish {
    version: Version,
    request: PublishViewV1,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    version: Version,
    destination: String,
    publication_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Withdraw {
    version: Version,
    destination: String,
    publication_id: String,
    occurrence_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retract {
    version: Version,
    origin: ViewOrigin,
    occurrence_id: String,
}
fn parse<T: serde::de::DeserializeOwned>(payload: Value) -> Result<T, BrainError> {
    serde_json::from_value(payload)
        .map_err(|_| BrainError::Invalid("invalid source-view-publication/1 request".into()))
}
impl Brain {
    /// Tokens are provided by CLI environment handles or a native authenticated
    /// session, never by the operation payload. Ordinary local trust is not an
    /// alternate credential for this explicit publication boundary.
    pub async fn source_view_operation(
        &self,
        operation: &str,
        payload: Value,
        origin_credential: Option<String>,
        destination_credential: Option<String>,
    ) -> Result<Value, BrainError> {
        match operation {
            "publish" => {
                let Publish {
                    version: Version::V1,
                    request,
                } = parse(payload)?;
                let receipt = self
                    .publish_source_view(
                        request,
                        PublicationCredentials::new(
                            origin_credential.ok_or_else(unavailable)?,
                            destination_credential.ok_or_else(unavailable)?,
                        ),
                    )
                    .await?;
                serde_json::to_value(receipt).map_err(|_| {
                    BrainError::Storage("publication receipt serialization failed".into())
                })
            }
            "read" => {
                let Read {
                    version: Version::V1,
                    destination,
                    publication_id,
                } = parse(payload)?;
                let view = self
                    .read_published_view(
                        &destination,
                        &publication_id,
                        origin_credential
                            .or(destination_credential)
                            .ok_or_else(unavailable)?,
                    )
                    .await?;
                serde_json::to_value(view).map_err(|_| {
                    BrainError::Storage("publication view serialization failed".into())
                })
            }
            "withdraw" => {
                let Withdraw {
                    version: Version::V1,
                    destination,
                    publication_id,
                    occurrence_id,
                } = parse(payload)?;
                self.withdraw_published_view(
                    destination,
                    publication_id,
                    occurrence_id,
                    destination_credential
                        .or(origin_credential)
                        .ok_or_else(unavailable)?,
                )
                .await?;
                Ok(serde_json::json!({"withdrawn":true}))
            }
            "retract" => {
                let Retract {
                    version: Version::V1,
                    origin,
                    occurrence_id,
                } = parse(payload)?;
                self.retract_source_views(
                    origin,
                    occurrence_id,
                    origin_credential.ok_or_else(unavailable)?,
                )
                .await?;
                Ok(serde_json::json!({"retracted":true}))
            }
            _ => Err(BrainError::Invalid("unknown source view operation".into())),
        }
    }
}

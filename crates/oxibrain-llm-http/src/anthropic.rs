use async_trait::async_trait;
use oxibrain_ports::{BrainError, LlmPort, LlmRequest, LlmResponse};
use reqwest::Client;
use serde_json::{Value, json};

pub struct AnthropicLlm {
    api_key: String,
    model: String,
    client: Client,
}

impl AnthropicLlm {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl LlmPort for AnthropicLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        let mut body = json!({
            "model": if req.model.is_empty() { &self.model } else { &req.model },
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.prompt}],
        });
        if let Some(system) = req.system {
            body["system"] = json!(system);
        }
        if let Some(schema) = req.json_schema {
            body["tools"] = json!([{"name":"extract_claims","description":"Extract structured claims","input_schema":schema}]);
            body["tool_choice"] = json!({"type":"tool","name":"extract_claims"});
        }
        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| provider(true, e.to_string()))?;
        let status = response.status();
        let raw: Value = response
            .json()
            .await
            .map_err(|e| provider(true, e.to_string()))?;
        if !status.is_success() {
            return Err(provider(
                status == 429 || status.as_u16() >= 500,
                raw.to_string(),
            ));
        }
        let input = raw["content"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["type"] == "tool_use"))
            .and_then(|v| v.get("input"))
            .ok_or_else(|| provider(false, "missing tool_use content"))?;
        Ok(LlmResponse {
            text: serde_json::to_string(input).map_err(|e| provider(false, e.to_string()))?,
            raw,
        })
    }
}

fn provider(retryable: bool, message: impl Into<String>) -> BrainError {
    BrainError::Provider {
        retryable,
        message: message.into(),
    }
}

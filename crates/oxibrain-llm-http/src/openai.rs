use async_trait::async_trait;
use oxibrain_ports::{BrainError, LlmPort, LlmRequest, LlmResponse};
use reqwest::Client;
use serde_json::{Value, json};

pub struct OpenAiLlm {
    api_key: String,
    model: String,
    client: Client,
}
impl OpenAiLlm {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl LlmPort for OpenAiLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        let mut messages = Vec::new();
        if let Some(system) = req.system {
            messages.push(json!({"role":"system","content":system}));
        }
        messages.push(json!({"role":"user","content":req.prompt}));
        let mut body = json!({"model": if req.model.is_empty() { &self.model } else { &req.model }, "max_tokens": req.max_tokens, "messages": messages});
        if let Some(schema) = req.json_schema {
            body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"extraction","schema":schema,"strict":true}});
        }
        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("authorization", format!("Bearer {}", self.api_key))
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
        let text = raw["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| provider(false, "missing response content"))?
            .to_owned();
        Ok(LlmResponse { text, raw })
    }
}
fn provider(retryable: bool, message: impl Into<String>) -> BrainError {
    BrainError::Provider {
        retryable,
        message: message.into(),
    }
}

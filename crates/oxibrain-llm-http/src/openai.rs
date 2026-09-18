use async_trait::async_trait;
use oxibrain_ports::{BrainError, LlmCapabilities, LlmPort, LlmRequest, LlmResponse};
use reqwest::Client;
use serde_json::{Value, json};

pub struct OpenAiLlm {
    base_url: String,
    api_key: Option<String>,
    model: String,
    client: Client,
}
impl OpenAiLlm {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url("https://api.openai.com/v1".into(), Some(api_key), model)
    }

    /// OpenAI-compatible endpoint on an arbitrary base — a loopback MLX
    /// server (LM Studio's MLX engine, `mlx_lm.server`) or a llama.cpp
    /// `server`. `base_url` is normalized (trailing slash stripped) and
    /// `{base_url}/chat/completions` is posted to. `api_key` is `None` for
    /// servers that need no auth; a bearer header is sent only when a key
    /// is present.
    pub fn with_base_url(base_url: String, api_key: Option<String>, model: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
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
        let url = format!("{}/chat/completions", self.base_url);
        let mut request = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let response = request
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

    fn capabilities(&self) -> LlmCapabilities {
        // OpenAI adapter emits `response_format: json_schema` (strict) when
        // a schema is supplied, and the chat-completions endpoint exposes
        // native tool calls as well. Profile validation treats these
        // equivalently via `LlmCapabilities::satisfies`.
        LlmCapabilities {
            grammar: false,
            structured_output: true,
            tool_call: true,
            json_schema: true,
        }
    }
}
fn provider(retryable: bool, message: impl Into<String>) -> BrainError {
    BrainError::Provider {
        retryable,
        message: message.into(),
    }
}

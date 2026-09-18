//! Loopback OpenAI-compatible adapter tests: `OpenAiLlm::with_base_url`
//! against a hand-rolled HTTP server. Proves the MLX serving path (LM
//! Studio's MLX engine, `mlx_lm.server`, llama.cpp `server`): base URL
//! routing, optional auth, and strict json_schema structured output.

use oxibrain_llm_http::OpenAiLlm;
use oxibrain_ports::{LlmPort, LlmRequest};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One recorded HTTP request: target path and body.
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    authorization: Option<String>,
    body: Value,
}

type Recorder = Arc<Mutex<Vec<Recorded>>>;

/// Spawn a one-JSON-response HTTP server on 127.0.0.1:0. Returns the base
/// URL and a recorder of every request it served.
async fn spawn_server(reply_content: &'static str) -> (String, Recorder) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recorded: Recorder = Arc::new(Mutex::new(Vec::new()));
    let server_recorder = recorded.clone();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let recorder = server_recorder.clone();
            tokio::spawn(async move {
                // Read until end of headers, then exactly content-length
                // body bytes.
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "client closed before headers completed");
                    buf.extend_from_slice(&chunk[..n]);
                    match find_header_end(&buf) {
                        Some(end) => break end,
                        None => continue,
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                let body_start = header_end + 4;
                while buf.len() < body_start + content_length {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "client closed before body completed");
                    buf.extend_from_slice(&chunk[..n]);
                }

                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
                let authorization = lines
                    .find_map(|l| {
                        let (name, value) = l.split_once(':')?;
                        name.eq_ignore_ascii_case("authorization")
                            .then(|| value.trim().to_string())
                    })
                    .filter(|v| !v.is_empty());
                let body: Value = serde_json::from_slice(&buf[body_start..]).unwrap();
                recorder.lock().push(Recorded {
                    path,
                    authorization,
                    body,
                });

                let reply = json!({
                    "choices": [{"message": {"content": reply_content}}]
                });
                let payload = serde_json::to_vec(&reply).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                sock.write_all(response.as_bytes()).await.unwrap();
                sock.write_all(&payload).await.unwrap();
                sock.shutdown().await.unwrap();
            });
        }
    });

    (format!("http://{addr}"), recorded)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn request(model: &str) -> LlmRequest {
    LlmRequest {
        model: model.into(),
        system: Some("extractor".into()),
        prompt: "Alice works on ProjectX".into(),
        json_schema: Some(json!({"type": "object"})),
        max_tokens: 512,
    }
}

#[tokio::test]
async fn loopback_routes_without_auth_and_sends_strict_schema() {
    let (base, recorded) = spawn_server(r#"{"claims":[]}"#).await;
    let llm = OpenAiLlm::with_base_url(base.clone(), None, "mlx-model".into());

    let response = llm.complete(request("")).await.unwrap();
    assert_eq!(response.text, r#"{"claims":[]}"#);

    let req = recorded.lock().pop().expect("one recorded request");
    assert_eq!(req.path, "/chat/completions");
    // No API key configured: no bearer header leaks onto the wire.
    assert_eq!(req.authorization, None);
    assert_eq!(req.body["model"], "mlx-model");
    assert_eq!(
        req.body["response_format"]["json_schema"]["strict"],
        json!(true)
    );
    assert_eq!(req.body["messages"][0]["role"], "system");
}

#[tokio::test]
async fn loopback_sends_bearer_only_when_key_present_and_prefers_request_model() {
    let (base, recorded) = spawn_server(r#"{"claims":[{"predicate":"works_on"}]}"#).await;
    let llm = OpenAiLlm::with_base_url(base, Some("secret".into()), "mlx-model".into());

    let response = llm.complete(request("override-model")).await.unwrap();
    assert!(response.text.contains("works_on"));

    let req = recorded.lock().pop().unwrap();
    assert_eq!(req.authorization, Some("Bearer secret".into()));
    // A non-empty request model overrides the adapter default.
    assert_eq!(req.body["model"], "override-model");
}

#[tokio::test]
async fn server_error_maps_to_provider_error_with_status_based_retryability() {
    // A 500 must map to a retryable provider error.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let _ = sock.read(&mut buf).await.unwrap();
        sock.write_all(
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        sock.shutdown().await.unwrap();
    });

    let llm = OpenAiLlm::with_base_url(format!("http://{addr}"), None, "m".into());
    let err = llm.complete(request("m")).await.unwrap_err();
    match err {
        oxibrain_ports::BrainError::Provider { retryable, .. } => assert!(retryable),
        other => panic!("expected Provider error, got {other:?}"),
    }
}

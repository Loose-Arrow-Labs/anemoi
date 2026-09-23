use futures::stream::{BoxStream, StreamExt};
use reqwest::Url;
use std::time::Duration;

/// Where a forwarded chat completion should go. `auth_token` originates from
/// the environment (expanded at config load) and is never committed.
#[derive(Debug, Clone)]
pub struct ForwardTarget {
    pub base_url: String,
    pub auth_token: Option<String>,
}

/// A chat-completion response forwarded back from a runtime. The body is a
/// stream so the caller can relay it without buffering (the inference gateway
/// requirement).
pub struct ForwardedChatCompletion {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: BoxStream<'static, Result<Vec<u8>, crate::RuntimeError>>,
}

const FORWARD_TIMEOUT: Duration = Duration::from_secs(120);

/// Forwards an OpenAI-style chat completion `body` to a runtime's
/// `/v1/chat/completions` endpoint, injecting the runtime bearer token when one
/// is configured. The response body is streamed straight through; only
/// transport failures (connection refused, timeout) surface as an `Err`.
pub async fn forward_chat_completion(
    target: &ForwardTarget,
    body: &serde_json::Value,
) -> Result<ForwardedChatCompletion, crate::RuntimeError> {
    let base = Url::parse(&target.base_url)
        .map_err(|error| crate::RuntimeError::Url(error.to_string()))?;
    let url = base
        .join("/v1/chat/completions")
        .map_err(|error| crate::RuntimeError::Url(error.to_string()))?;

    let client = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .build()?;
    let mut request = client.post(url).json(body);
    if let Some(token) = &target.auth_token {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .bytes_stream()
        .map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(crate::RuntimeError::from)
        })
        .boxed();

    Ok(ForwardedChatCompletion {
        status,
        content_type,
        body,
    })
}

/// Builds a deterministic OpenAI-style SSE response for the mock runtime. The
/// payload echoes `selected_model`, so a test can confirm the gateway rewrote
/// the `model` field to the decision's selected model. No network is involved.
pub fn mock_chat_completion(selected_model: &str) -> ForwardedChatCompletion {
    let chunk = format!(
        "data: {{\"object\":\"chat.completion.chunk\",\"model\":\"{selected_model}\",\
\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"mock response from {selected_model}\"}}}}]}}\n\n"
    );
    let chunks: Vec<Result<Vec<u8>, crate::RuntimeError>> =
        vec![Ok(chunk.into_bytes()), Ok(b"data: [DONE]\n\n".to_vec())];
    ForwardedChatCompletion {
        status: 200,
        content_type: Some("text/event-stream".to_string()),
        body: futures::stream::iter(chunks).boxed(),
    }
}

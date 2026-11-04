//! Model Gateway: a normalized streaming request and the OpenAI-compatible Chat Completions
//! adapter. Provider-specific wire formats stay inside adapters; the run driver only sees
//! `ModelRequest`, `Delta`, and `ModelResponse`.
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, time::Duration};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub tools: Vec<Value>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    pub total: u64,
}

impl Usage {
    pub fn to_value(self) -> Value {
        json!({
            "input": self.input,
            "cached": self.cached,
            "output": self.output,
            "reasoning": self.reasoning,
            "total": self.total
        })
    }

    pub fn from_value(value: &Value) -> Self {
        let number = |key: &str| value[key].as_u64().unwrap_or_default();
        Self {
            input: number("input"),
            cached: number("cached"),
            output: number("output"),
            reasoning: number("reasoning"),
            total: number("total"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallRequest>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

pub enum Delta {
    Text(String),
    Reasoning(String),
}

#[derive(Debug)]
pub struct ModelError {
    pub message: String,
    /// True only when no output reached the caller, so a new attempt cannot duplicate it.
    pub retryable: bool,
}

pub struct ModelInfo {
    pub id: String,
    pub context_window: Option<u64>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type OnDelta<'a> = &'a (dyn Fn(Delta) + Send + Sync);

pub trait ModelClient: Send + Sync {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>>;
    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: OnDelta<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>>;
}

pub struct OpenAiCompatible {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    models: Option<Vec<String>>,
}

impl OpenAiCompatible {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        models: Option<Vec<String>>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            models,
        })
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, String> {
        if let Some(models) = &self.models {
            return Ok(models
                .iter()
                .map(|id| ModelInfo {
                    id: id.clone(),
                    context_window: None,
                })
                .collect());
        }
        let response = self
            .authorize(self.client.get(format!("{}/models", self.base_url)))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("Model list request failed: {e}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("Model list response is not JSON: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "Model list failed with {status}: {}",
                error_message(&body)
            ));
        }
        let mut models: Vec<ModelInfo> = body["data"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|model| {
                Some(ModelInfo {
                    id: model["id"].as_str()?.to_owned(),
                    context_window: model["context_length"]
                        .as_u64()
                        .or_else(|| model["context_window"].as_u64()),
                })
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    async fn chat(
        &self,
        request: &ModelRequest,
        on_delta: OnDelta<'_>,
    ) -> Result<ModelResponse, ModelError> {
        let fatal = |message: String| ModelError {
            message,
            retryable: false,
        };
        let mut body = json!({
            "model": request.model,
            "messages": request.messages,
            "stream": true,
            "stream_options": {"include_usage": true}
        });
        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools);
        }
        if let Some(effort) = &request.effort {
            body["reasoning_effort"] = json!(effort);
        }
        let response = self
            .authorize(
                self.client
                    .post(format!("{}/chat/completions", self.base_url))
                    .json(&body),
            )
            .send()
            .await
            .map_err(|e| ModelError {
                message: format!("Model request failed: {e}"),
                retryable: true,
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text)
                .map(|body| error_message(&body))
                .unwrap_or_else(|_| text.chars().take(2000).collect());
            return Err(ModelError {
                message: format!("Model request failed with {status}: {detail}"),
                retryable: status.as_u16() == 429 || status.is_server_error(),
            });
        }
        let mut stream = response.bytes_stream();
        let mut parser = SseParser::default();
        let mut produced = false;
        loop {
            let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
                Err(_) => {
                    return Err(ModelError {
                        message: "The model stream stopped responding".into(),
                        retryable: !produced,
                    })
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return Err(ModelError {
                        message: format!("The model stream failed: {e}"),
                        retryable: !produced,
                    })
                }
                Ok(Some(Ok(chunk))) => chunk,
            };
            for data in parser.push(&chunk) {
                if data == "[DONE]" {
                    return Ok(parser.finish());
                }
                let value: Value = serde_json::from_str(&data)
                    .map_err(|e| fatal(format!("Malformed model stream event: {e}")))?;
                if value.get("error").is_some_and(|e| !e.is_null()) {
                    return Err(fatal(format!(
                        "The model reported an error: {}",
                        error_message(&value)
                    )));
                }
                for delta in parser.apply(&value) {
                    produced = true;
                    on_delta(delta);
                }
            }
        }
        if parser.saw_event {
            Ok(parser.finish())
        } else {
            Err(ModelError {
                message: "The model stream ended without a response".into(),
                retryable: true,
            })
        }
    }
}

impl ModelClient for OpenAiCompatible {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>> {
        Box::pin(self.list())
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: OnDelta<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(self.chat(request, on_delta))
    }
}

fn error_message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .or_else(|| body["message"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| body.to_string().chars().take(2000).collect())
}

/// Incremental Server-Sent Events decoder and Chat Completions chunk accumulator.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
    response: ModelResponse,
    saw_event: bool,
}

impl SseParser {
    /// Feed raw bytes and return the complete `data` payloads they finish.
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
        events
    }

    fn apply(&mut self, chunk: &Value) -> Vec<Delta> {
        self.saw_event = true;
        let mut deltas = Vec::new();
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            let number = |value: &Value| value.as_u64().unwrap_or_default();
            let input = number(&usage["prompt_tokens"]);
            let output = number(&usage["completion_tokens"]);
            self.response.usage = Some(Usage {
                input,
                cached: number(&usage["prompt_tokens_details"]["cached_tokens"]),
                output,
                reasoning: number(&usage["completion_tokens_details"]["reasoning_tokens"]),
                total: usage["total_tokens"].as_u64().unwrap_or(input + output),
            });
        }
        let Some(choice) = chunk["choices"].get(0) else {
            return deltas;
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
            self.response.text.push_str(text);
            deltas.push(Delta::Text(text.to_owned()));
        }
        if let Some(text) = delta["reasoning_content"]
            .as_str()
            .or_else(|| delta["reasoning"].as_str())
            .filter(|text| !text.is_empty())
        {
            self.response.reasoning.push_str(text);
            deltas.push(Delta::Reasoning(text.to_owned()));
        }
        for call in delta["tool_calls"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let index = call["index"]
                .as_u64()
                .map(|index| index as usize)
                .unwrap_or(self.response.tool_calls.len());
            while self.response.tool_calls.len() <= index {
                self.response.tool_calls.push(ToolCallRequest::default());
            }
            let entry = &mut self.response.tool_calls[index];
            if let Some(id) = call["id"].as_str() {
                entry.id = id.to_owned();
            }
            if let Some(name) = call["function"]["name"].as_str() {
                entry.name.push_str(name);
            }
            if let Some(arguments) = call["function"]["arguments"].as_str() {
                entry.arguments.push_str(arguments);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.response.finish_reason = Some(reason.to_owned());
        }
        deltas
    }

    fn finish(&mut self) -> ModelResponse {
        let mut response = std::mem::take(&mut self.response);
        response.tool_calls.retain(|call| !call.name.is_empty());
        for (index, call) in response.tool_calls.iter_mut().enumerate() {
            if call.id.is_empty() {
                call.id = format!("call_{index}");
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_chunks_accumulate_text_reasoning_tools_and_usage() {
        let mut parser = SseParser::default();
        let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\",\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4,\"total_tokens\":14}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut deltas = 0;
        let mut done = false;
        // Split mid-line to exercise buffering across network chunks.
        for piece in stream.as_bytes().chunks(7) {
            for data in parser.push(piece) {
                if data == "[DONE]" {
                    done = true;
                } else {
                    deltas += parser.apply(&serde_json::from_str(&data).unwrap()).len();
                }
            }
        }
        assert!(done);
        assert_eq!(deltas, 3);
        let response = parser.finish();
        assert_eq!(response.text, "Hello");
        assert_eq!(response.reasoning, "think");
        assert_eq!(
            response.tool_calls,
            vec![ToolCallRequest {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"a\"}".into()
            }]
        );
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(response.usage.unwrap().total, 14);
    }
}

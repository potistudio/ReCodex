//! Test doubles for the Model Gateway, shared by harness, host, LAN, and Web UI tests.
use super::{
    model::{
        BoxFuture, Delta, ModelClient, ModelError, ModelInfo, ModelRequest, ModelResponse, OnDelta,
        ToolCallRequest, Usage,
    },
    Harness,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) enum Step {
    Reply(ModelResponse),
    /// Never answers; used to exercise cancellation while a request is in flight.
    Hang,
}

#[derive(Default)]
pub(crate) struct Scripted {
    steps: Mutex<VecDeque<Step>>,
    pub requests: Mutex<Vec<Vec<Value>>>,
}

impl Scripted {
    pub fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::default(),
        })
    }
}

impl ModelClient for Scripted {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>> {
        Box::pin(async {
            Ok(vec![ModelInfo {
                id: "test-model".into(),
                context_window: Some(1000),
                route: Default::default(),
            }])
        })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: OnDelta<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.messages.clone());
            let step = self.steps.lock().unwrap().pop_front();
            match step {
                Some(Step::Reply(response)) => {
                    if !response.text.is_empty() {
                        on_delta(Delta::Text(response.text.clone()));
                    }
                    Ok(response)
                }
                Some(Step::Hang) => std::future::pending().await,
                None => Err(ModelError::new("script exhausted", false)),
            }
        })
    }
}

pub(crate) fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCallRequest {
    ToolCallRequest {
        id: id.into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

pub(crate) fn reply(text: &str, tool_calls: Vec<ToolCallRequest>) -> Step {
    Step::Reply(ModelResponse {
        text: text.into(),
        tool_calls,
        usage: Some(Usage {
            input: 10,
            output: 5,
            total: 15,
            ..Usage::default()
        }),
        ..ModelResponse::default()
    })
}

/// Asks to run a command on every prompt and answers once the call has a result, so each
/// turn produces exactly one approval request.
pub(crate) struct ApprovalModel;

impl ModelClient for ApprovalModel {
    fn models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, String>> {
        Box::pin(async {
            Ok(vec![ModelInfo {
                id: "test-model".into(),
                context_window: None,
                route: Default::default(),
            }])
        })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        _on_delta: OnDelta<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            let answered = request
                .messages
                .last()
                .is_some_and(|message| message["role"] == "tool");
            Ok(ModelResponse {
                text: if answered {
                    "Done.".into()
                } else {
                    String::new()
                },
                tool_calls: if answered {
                    vec![]
                } else {
                    vec![tool_call(
                        &format!("call-{}", request.messages.len()),
                        "run_command",
                        json!({"command":"echo test"}),
                    )]
                },
                ..ModelResponse::default()
            })
        })
    }
}

/// A harness whose every turn waits for one command approval.
pub(crate) fn approval_harness(path: &Path) -> Arc<Harness> {
    Harness::with_client(path, Arc::new(ApprovalModel), None)
}

/// A recorded HTTP request: request line, lowercased headers, and body.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: String,
    pub body: String,
}

/// A local HTTP server answering each request with `handler(method, path) -> (status line,
/// content type, body)` and recording what it received.
pub(crate) async fn mock_http(
    handler: impl Fn(&str, &str) -> (&'static str, &'static str, String) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                let (head, length) = loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (end, length);
                    }
                };
                while request.len() < head + 4 + length {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let mut start = text.lines().next().unwrap_or_default().split(' ');
                let method = start.next().unwrap_or_default().to_owned();
                let path = start.next().unwrap_or_default().to_owned();
                log.lock().unwrap().push(Recorded {
                    method: method.clone(),
                    path: path.clone(),
                    headers: text[..head].to_lowercase(),
                    body: text[head + 4..].to_owned(),
                });
                let (status, content_type, body) = handler(&method, &path);
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (address, seen)
}

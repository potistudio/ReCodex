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
                None => Err(ModelError {
                    message: "script exhausted".into(),
                    retryable: false,
                }),
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

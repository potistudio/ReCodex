//! Run Driver: sequences steps (one model request plus its tool calls), attempts, approvals,
//! and run termination. Facts are committed to the ledger before they are broadcast.
use super::{
    context,
    ledger::{kind, Ledger},
    model::{Delta, ModelRequest, ModelResponse, ToolCallRequest, Usage},
    policy::{self, ApprovalKind, Decision, Grants},
    tools::{self, Outcome, PreparedCall},
    Harness,
};
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

const MAX_ATTEMPTS: u32 = 3;

pub(super) struct RunSpec {
    pub session: String,
    pub run_id: String,
    pub cwd: String,
    /// Provider-native model ID (without the ReCodex prefix).
    pub model: String,
    pub effort: Option<String>,
    pub cancel: watch::Receiver<bool>,
}

enum Finish {
    Completed,
    Interrupted,
}

pub(super) async fn run(harness: Arc<Harness>, spec: RunSpec) {
    let result = drive(&harness, &spec).await;
    let (status, error) = match result {
        Ok(Finish::Completed) => ("completed", None),
        Ok(Finish::Interrupted) => ("interrupted", None),
        Err(error) => ("failed", Some(error)),
    };
    let recorded = harness.ledger().and_then(|ledger| {
        ledger.append(
            &spec.session,
            None,
            kind::RUN_FINISHED,
            json!({"runId":spec.run_id,"status":status,"error":error}),
        )
    });
    let (status, error) = match recorded {
        Ok(_) => (status, error),
        Err(e) => (
            "failed",
            Some(format!("{}; {e}", error.unwrap_or_default())),
        ),
    };
    harness.release_run(&spec.session, &spec.run_id);
    let mut turn = json!({"id":spec.run_id,"status":status,"items":[]});
    if let Some(error) = error {
        turn["error"] = json!({"message":error});
    }
    harness.emit(json!({"method":"turn/completed","params":{"threadId":spec.session,"turn":turn}}));
}

fn cancelled(spec: &RunSpec) -> bool {
    *spec.cancel.borrow()
}

async fn wait_cancel(spec: &RunSpec) {
    let mut cancel = spec.cancel.clone();
    let _ = cancel.wait_for(|cancelled| *cancelled).await;
}

async fn drive(harness: &Arc<Harness>, spec: &RunSpec) -> Result<Finish, String> {
    let ledger = harness.ledger()?;
    let definitions = tools::registry();
    let schemas = tools::schemas(&definitions);
    let names: Vec<_> = definitions.iter().map(|tool| tool.name).collect();
    for index in 0..harness.max_steps {
        if cancelled(spec) {
            return Ok(Finish::Interrupted);
        }
        let events = ledger.events(&spec.session)?;
        let through = events.last().map_or(0, |event| event.seq);
        let step_id = Uuid::new_v4().to_string();
        ledger.append(
            &spec.session,
            None,
            kind::STEP_STARTED,
            json!({
                "runId":spec.run_id,
                "stepId":step_id,
                "index":index,
                "model":spec.model,
                "effort":spec.effort,
                "contextVersion":context::VERSION,
                "contextThrough":through,
                "tools":names,
                "policy":policy::VERSION
            }),
        )?;
        let request = ModelRequest {
            model: spec.model.clone(),
            messages: context::messages(&events, &spec.cwd, through),
            tools: schemas.clone(),
            effort: spec.effort.clone(),
            cwd: spec.cwd.clone(),
            session: spec.session.clone(),
        };
        let item_id = format!("{}-{index}-message", spec.run_id);
        let Some((attempt_id, response)) =
            request_model(harness, ledger, spec, &step_id, &item_id, &request).await?
        else {
            return Ok(Finish::Interrupted);
        };
        let usage = response.usage.unwrap_or_default();
        ledger.append(
            &spec.session,
            None,
            kind::MESSAGE_ASSISTANT,
            json!({
                "runId":spec.run_id,
                "stepId":step_id,
                "attemptId":attempt_id,
                "itemId":item_id,
                "text":response.text,
                "reasoning":response.reasoning,
                "toolCalls":response.tool_calls.iter().map(|call| json!({
                    "id":call.id,"name":call.name,"arguments":call.arguments
                })).collect::<Vec<_>>(),
                "finishReason":response.finish_reason,
                "usage":usage.to_value()
            }),
        )?;
        if !response.text.is_empty() {
            harness.emit(json!({"method":"item/completed","params":{
                "threadId":spec.session,
                "turnId":spec.run_id,
                "item":{"id":item_id,"type":"agentMessage","text":response.text}
            }}));
        }
        if response.usage.is_some() {
            emit_usage(harness, spec, context::usage_total(&events), usage);
        }
        if response.tool_calls.is_empty() {
            return Ok(Finish::Completed);
        }
        if run_tools(harness, ledger, spec, &step_id, index, &response.tool_calls).await? {
            return Ok(Finish::Interrupted);
        }
    }
    Err(format!(
        "Stopped after reaching the step limit ({})",
        harness.max_steps
    ))
}

/// One model request with bounded retries. `None` means the run was cancelled.
async fn request_model(
    harness: &Arc<Harness>,
    ledger: &Ledger,
    spec: &RunSpec,
    step_id: &str,
    item_id: &str,
    request: &ModelRequest,
) -> Result<Option<(String, ModelResponse)>, String> {
    let client = harness.client()?;
    for attempt in 1..=MAX_ATTEMPTS {
        let attempt_id = Uuid::new_v4().to_string();
        let partial = StdMutex::new(String::new());
        let on_delta = |delta: Delta| match delta {
            Delta::Text(text) => {
                partial.lock().unwrap().push_str(&text);
                harness.emit(json!({"method":"item/agentMessage/delta","params":{
                    "threadId":spec.session,"turnId":spec.run_id,"itemId":item_id,"delta":text
                }}));
            }
            Delta::Reasoning(text) => {
                harness.emit(json!({"method":"item/reasoning/summaryTextDelta","params":{
                    "threadId":spec.session,"turnId":spec.run_id,
                    "itemId":format!("{item_id}-reasoning"),"summaryIndex":0,"delta":text
                }}));
            }
        };
        let result = tokio::select! {
            result = client.stream(request, &on_delta) => Some(result),
            _ = wait_cancel(spec) => None,
        };
        let failure = |error: &str, retryable: bool, cancelled: bool| {
            json!({
                "runId":spec.run_id,
                "stepId":step_id,
                "attemptId":attempt_id,
                "attempt":attempt,
                "error":error,
                "retryable":retryable,
                "cancelled":cancelled,
                "partialText":*partial.lock().unwrap()
            })
        };
        match result {
            None => {
                ledger.append(
                    &spec.session,
                    None,
                    kind::ATTEMPT_FAILED,
                    failure("The turn was interrupted", false, true),
                )?;
                return Ok(None);
            }
            Some(Ok(response)) => return Ok(Some((attempt_id, response))),
            Some(Err(error)) => {
                ledger.append(
                    &spec.session,
                    None,
                    kind::ATTEMPT_FAILED,
                    failure(&error.message, error.retryable, false),
                )?;
                if !error.retryable || attempt == MAX_ATTEMPTS {
                    return Err(error.message);
                }
                let backoff = Duration::from_millis(500 * 2u64.pow(attempt - 1));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = wait_cancel(spec) => return Ok(None),
                }
            }
        }
    }
    unreachable!("the last attempt always returns")
}

fn emit_usage(harness: &Harness, spec: &RunSpec, before: Usage, last: Usage) {
    let breakdown = |usage: Usage| {
        json!({
            "totalTokens":usage.total,
            "inputTokens":usage.input,
            "cachedInputTokens":usage.cached,
            "cacheWriteInputTokens":0,
            "outputTokens":usage.output,
            "reasoningOutputTokens":usage.reasoning
        })
    };
    let total = Usage {
        input: before.input + last.input,
        cached: before.cached + last.cached,
        output: before.output + last.output,
        reasoning: before.reasoning + last.reasoning,
        total: before.total + last.total,
    };
    harness.emit(json!({"method":"thread/tokenUsage/updated","params":{
        "threadId":spec.session,
        "turnId":spec.run_id,
        "tokenUsage":{"total":breakdown(total),"last":breakdown(last),"modelContextWindow":Value::Null}
    }}));
}

struct Slot {
    call: ToolCallRequest,
    item_id: String,
    prepared: Option<PreparedCall>,
}

struct Tools<'a> {
    harness: &'a Arc<Harness>,
    ledger: &'a Ledger,
    spec: &'a RunSpec,
    step_id: &'a str,
}

impl Tools<'_> {
    fn record(&self, kind: &str, mut payload: Value) -> Result<(), String> {
        payload["runId"] = json!(self.spec.run_id);
        payload["stepId"] = json!(self.step_id);
        self.ledger
            .append(&self.spec.session, None, kind, payload)
            .map(|_| ())
    }

    fn item_event(&self, method: &str, item: &Value) {
        self.harness.emit(json!({"method":method,"params":{
            "threadId":self.spec.session,"turnId":self.spec.run_id,"item":item
        }}));
    }

    /// Commit the terminal result of a call, then show it.
    fn complete(
        &self,
        slot: &Slot,
        status: &str,
        content: String,
        outcome: Option<&Outcome>,
    ) -> Result<(), String> {
        let item_status = match status {
            "completed" => "completed",
            "denied" => "declined",
            "cancelled" => "cancelled",
            _ => "failed",
        };
        let item = tools::display_item(
            &slot.item_id,
            &slot.call,
            slot.prepared.as_ref(),
            item_status,
            outcome,
        );
        self.record(
            kind::TOOL_COMPLETED,
            json!({
                "callId":slot.call.id,
                "status":status,
                "content":content,
                "output":outcome.map(|outcome| outcome.output.as_str()),
                "exitCode":outcome.and_then(|outcome| outcome.exit_code),
                "item":item
            }),
        )?;
        self.item_event("item/completed", &item);
        Ok(())
    }

    fn request_approval(
        &self,
        slot: &Slot,
        prepared: &PreparedCall,
        approval: ApprovalKind,
    ) -> Result<(String, tokio::sync::oneshot::Receiver<String>), String> {
        let request_id = format!("rx-approval-{}", Uuid::new_v4());
        let receiver =
            self.harness
                .register_approval(&request_id, &self.spec.session, &self.spec.run_id);
        self.record(
            kind::APPROVAL_REQUESTED,
            json!({
                "callId":slot.call.id,
                "requestId":request_id,
                "approvalKind":approval.as_str(),
                "fingerprint":prepared.fingerprint
            }),
        )?;
        let decisions = json!(["accept", "acceptForSession", "decline"]);
        let event = match approval {
            ApprovalKind::Command => json!({
                "id":request_id,
                "method":"item/commandExecution/requestApproval",
                "params":{
                    "threadId":self.spec.session,
                    "turnId":self.spec.run_id,
                    "itemId":slot.item_id,
                    "command":prepared.args["command"],
                    "cwd":self.spec.cwd,
                    "reason":"The ReCodex harness wants to run a command without a sandbox.",
                    "availableDecisions":decisions
                }
            }),
            ApprovalKind::FileChange => json!({
                "id":request_id,
                "method":"item/fileChange/requestApproval",
                "params":{
                    "threadId":self.spec.session,
                    "turnId":self.spec.run_id,
                    "itemId":slot.item_id,
                    "reason":format!("The ReCodex harness wants to change {}.", prepared.path.as_deref().unwrap_or_default()),
                    "availableDecisions":decisions
                }
            }),
        };
        self.harness.emit(event);
        Ok((request_id, receiver))
    }

    async fn dispatch(&self, slot: &Slot, root: &Path) -> Result<(), String> {
        let prepared = slot
            .prepared
            .as_ref()
            .expect("dispatched calls are prepared");
        self.record(
            kind::TOOL_DISPATCHED,
            json!({"callId":slot.call.id,"fingerprint":prepared.fingerprint}),
        )?;
        let last_update = StdMutex::new(Instant::now() - Duration::from_secs(1));
        let on_output = |output: &str| {
            let mut last = last_update.lock().unwrap();
            if last.elapsed() < Duration::from_millis(100) {
                return;
            }
            *last = Instant::now();
            let outcome = Outcome {
                failed: false,
                output: output.to_owned(),
                diff: None,
                exit_code: None,
            };
            let item = tools::display_item(
                &slot.item_id,
                &slot.call,
                Some(prepared),
                "inProgress",
                Some(&outcome),
            );
            self.item_event("item/updated", &item);
        };
        let outcome = tools::execute(prepared, root, self.spec.cancel.clone(), &on_output).await;
        let content = if outcome.failed {
            format!("Error: {}", tools::model_output(&outcome.output))
        } else {
            tools::model_output(&outcome.output)
        };
        self.complete(
            slot,
            if outcome.failed {
                "failed"
            } else {
                "completed"
            },
            content,
            Some(&outcome),
        )
    }
}

/// Validate, authorize, schedule, and execute one step's tool calls. Returns true when the
/// run was cancelled; every call still receives a terminal result.
async fn run_tools(
    harness: &Arc<Harness>,
    ledger: &Ledger,
    spec: &RunSpec,
    step_id: &str,
    index: usize,
    calls: &[ToolCallRequest],
) -> Result<bool, String> {
    let context = Tools {
        harness,
        ledger,
        spec,
        step_id,
    };
    let root = PathBuf::from(&spec.cwd);
    let mut slots: Vec<Slot> = Vec::new();
    let mut done = vec![false; calls.len()];
    for (position, call) in calls.iter().enumerate() {
        let item_id = format!("{}-{index}-tool-{position}", spec.run_id);
        let prepared = tools::prepare(call, &root);
        let slot = Slot {
            call: call.clone(),
            item_id,
            prepared: prepared.as_ref().ok().cloned(),
        };
        match prepared {
            Ok(prepared) => {
                let item =
                    tools::display_item(&slot.item_id, call, Some(&prepared), "inProgress", None);
                context.record(
                    kind::TOOL_STARTED,
                    json!({"callId":call.id,"name":call.name,"item":item}),
                )?;
                context.item_event("item/started", &item);
            }
            Err(error) => {
                // Record the call's position before its result so transcripts keep call order.
                let item = tools::display_item(&slot.item_id, call, None, "inProgress", None);
                context.record(
                    kind::TOOL_STARTED,
                    json!({"callId":call.id,"name":call.name,"item":item}),
                )?;
                context.complete(&slot, "failed", format!("Error: {error}"), None)?;
                done[position] = true;
            }
        }
        slots.push(slot);
    }

    let mut grants = Grants::from_events(&ledger.events(&spec.session)?);
    let mut approved = Vec::new();
    let mut waiting = Vec::new();
    for (position, slot) in slots.iter().enumerate() {
        let Some(prepared) = &slot.prepared else {
            continue;
        };
        let decision = policy::authorize(prepared, &grants);
        context.record(
            kind::TOOL_AUTHORIZED,
            json!({
                "callId":slot.call.id,
                "decision":decision.label(),
                "reason":match &decision { Decision::Deny(reason) => Some(reason.as_str()), _ => None },
                "policy":policy::VERSION,
                "fingerprint":prepared.fingerprint
            }),
        )?;
        match decision {
            Decision::Allow => approved.push(position),
            Decision::Deny(reason) => {
                context.complete(slot, "denied", format!("Denied by policy: {reason}"), None)?;
                done[position] = true;
            }
            Decision::Ask(approval) => {
                let (request_id, receiver) = context.request_approval(slot, prepared, approval)?;
                waiting.push((position, approval, request_id, receiver));
            }
        }
    }
    // All approval requests are outstanding at once; answers are applied in call order.
    for (position, approval, request_id, receiver) in waiting {
        let slot = &slots[position];
        let decision = if cancelled(spec) {
            "cancel".to_owned()
        } else {
            tokio::select! {
                answer = receiver => answer.unwrap_or_else(|_| "cancel".into()),
                _ = wait_cancel(spec) => "cancel".into(),
            }
        };
        harness.approvals.lock().unwrap().remove(&request_id);
        context.record(
            kind::APPROVAL_RESOLVED,
            json!({
                "callId":slot.call.id,
                "requestId":request_id,
                "approvalKind":approval.as_str(),
                "decision":decision
            }),
        )?;
        match decision.as_str() {
            "accept" => approved.push(position),
            "acceptForSession" => {
                grants.grant(approval);
                approved.push(position);
            }
            "decline" => {
                context.complete(
                    slot,
                    "denied",
                    "The user declined this action.".into(),
                    None,
                )?;
                done[position] = true;
            }
            _ => {
                context.complete(
                    slot,
                    "cancelled",
                    "The turn was interrupted before this call ran.".into(),
                    None,
                )?;
                done[position] = true;
            }
        }
    }
    approved.sort_unstable();
    // Revalidate right before dispatch: the approved action must still resolve identically.
    let mut runnable = Vec::new();
    for position in approved {
        let slot = &slots[position];
        let fingerprint = slot.prepared.as_ref().map(|p| p.fingerprint.clone());
        match tools::prepare(&slot.call, &root) {
            Ok(again) if Some(&again.fingerprint) == fingerprint.as_ref() => {
                runnable.push(position)
            }
            _ => {
                context.complete(
                    slot,
                    "failed",
                    "Error: the call no longer matches what was authorized".into(),
                    None,
                )?;
                done[position] = true;
            }
        }
    }
    let prepared: Vec<PreparedCall> = runnable
        .iter()
        .map(|position| slots[*position].prepared.clone().unwrap())
        .collect();
    let by_prepared: HashMap<usize, usize> = runnable.iter().copied().enumerate().collect();
    for batch in tools::schedule(&prepared) {
        if cancelled(spec) {
            break;
        }
        let results = join_all(batch.iter().map(|index| {
            let position = by_prepared[index];
            let slot = &slots[position];
            let root = root.clone();
            let context = &context;
            async move { (position, context.dispatch(slot, &root).await) }
        }))
        .await;
        for (position, result) in results {
            result?;
            done[position] = true;
        }
    }
    for (position, slot) in slots.iter().enumerate() {
        if !done[position] {
            context.complete(
                slot,
                "cancelled",
                "The turn was interrupted before this call ran.".into(),
                None,
            )?;
        }
    }
    Ok(cancelled(spec))
}

/// Close runs left open by a previous process. A call that was dispatched without a recorded
/// result has an unknown outcome; it is reported as such and never re-executed.
pub(super) fn recover(ledger: &Ledger) -> Result<(), String> {
    for (session, run) in ledger.unfinished_runs()? {
        let events = ledger.events(&session)?;
        let of_run = |event: &&super::ledger::Event| event.payload["runId"] == run;
        let mut started: Vec<(String, Value)> = Vec::new();
        let mut finished = Vec::new();
        let mut dispatched = Vec::new();
        let mut requested = Vec::new();
        let mut resolved = Vec::new();
        for event in events.iter().filter(of_run) {
            let call = event.payload["callId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match event.kind.as_str() {
                kind::TOOL_STARTED => started.push((call, event.payload["item"].clone())),
                kind::TOOL_COMPLETED => finished.push(call),
                kind::TOOL_DISPATCHED => dispatched.push(call),
                kind::APPROVAL_REQUESTED => requested.push(event.payload.clone()),
                kind::APPROVAL_RESOLVED => resolved.push(
                    event.payload["requestId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                ),
                _ => {}
            }
        }
        for request in requested {
            let id = request["requestId"].as_str().unwrap_or_default();
            if !resolved.iter().any(|resolved| resolved == id) {
                ledger.append(
                    &session,
                    None,
                    kind::APPROVAL_RESOLVED,
                    json!({"runId":run,"callId":request["callId"],"requestId":id,
                        "approvalKind":request["approvalKind"],"decision":"expired"}),
                )?;
            }
        }
        for (call, mut item) in started {
            if finished.contains(&call) {
                continue;
            }
            let unknown = dispatched.contains(&call);
            item["status"] = json!(if unknown { "failed" } else { "cancelled" });
            ledger.append(
                &session,
                None,
                kind::TOOL_COMPLETED,
                json!({
                    "runId":run,
                    "callId":call,
                    "status":if unknown { "outcomeUnknown" } else { "cancelled" },
                    "content":if unknown {
                        "ReCodex stopped while this call was running; its outcome is unknown. Inspect the current state before retrying."
                    } else {
                        "The turn was interrupted before this call ran."
                    },
                    "item":item
                }),
            )?;
        }
        ledger.append(
            &session,
            None,
            kind::RUN_FINISHED,
            json!({"runId":run,"status":"interrupted","error":"ReCodex stopped before this turn finished"}),
        )?;
    }
    Ok(())
}

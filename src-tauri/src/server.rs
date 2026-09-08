use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex},
};

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
pub struct Server {
    child: Child,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: std::sync::atomic::AtomicU64,
}
impl Server {
    pub async fn start(emit: impl Fn(Value) + Send + Sync + 'static) -> Result<Self, String> {
        let executable = std::env::var("RECODEX_CODEX_PATH").unwrap_or_else(|_| "codex".into());
        let mut command = Command::new(executable);
        command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().map_err(|e| {
            format!("Codex could not start: {e}. Install Codex CLI or set RECODEX_CODEX_PATH.")
        })?;
        let stdin = child.stdin.take().ok_or("Codex stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("Codex stdout is unavailable")?;
        let stderr = child.stderr.take().ok_or("Codex stderr is unavailable")?;
        let pending: Pending = Arc::default();
        let responses = pending.clone();
        let emit = Arc::new(emit);
        let events = emit.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("method").is_some() {
                    events(message);
                } else if let Some(id) = message["id"].as_u64() {
                    if let Some(sender) = responses.lock().unwrap().remove(&id) {
                        let result = if let Some(error) = message.get("error") {
                            Err(error["message"]
                                .as_str()
                                .unwrap_or("App-server request failed")
                                .to_owned())
                        } else {
                            Ok(message["result"].clone())
                        };
                        let _ = sender.send(result);
                    }
                }
            }
            for (_, sender) in responses.lock().unwrap().drain() {
                let _ = sender.send(Err("Codex connection closed".into()));
            }
            events(json!({"method":"recodex/disconnected","params":{}}));
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                emit(json!({"method":"recodex/log","params":{"message":line}}));
            }
        });
        let server = Self {
            child,
            stdin: Mutex::new(stdin),
            pending,
            next_id: std::sync::atomic::AtomicU64::new(1),
        };
        server
            .request(
                "initialize",
                json!({"clientInfo":{"name":"recodex","title":"ReCodex","version":"0.1.0"}}),
            )
            .await?;
        server.write(json!({"method":"initialized"})).await?;
        Ok(server)
    }
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    pub async fn write(&self, message: Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&bytes).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        if let Err(error) = self
            .write(json!({"id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let response = tokio::time::timeout(Duration::from_secs(60), receiver).await;
        self.pending.lock().unwrap().remove(&id);
        response
            .map_err(|_| format!("{method} timed out; reconnect before retrying a message"))?
            .map_err(|_| "Codex connection closed".to_owned())?
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Requires an installed Codex CLI; performs no model turn"]
    async fn live_handshake_and_model_list() {
        let mut server = Server::start(|_| {}).await.unwrap();
        assert!(server.is_running());
        let models = server.request("model/list", json!({})).await.unwrap();
        assert!(models["data"].is_array());
        let account = server.request("account/read", json!({})).await.unwrap();
        assert!(account.get("requiresOpenaiAuth").is_some());
    }

    #[tokio::test]
    #[ignore = "Requires Codex sign-in and uses one small model turn"]
    async fn live_chat_turn() {
        let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
        let server = Server::start(move |event| {
            let _ = send.send(event);
        })
        .await
        .unwrap();
        let thread = server
            .request(
                "thread/start",
                json!({
                    "cwd": std::env::temp_dir(), "ephemeral": true,
                    "sandbox": "read-only", "approvalPolicy": "never"
                }),
            )
            .await
            .unwrap();
        let thread_id = thread["thread"]["id"].as_str().unwrap();
        server.request("turn/start", json!({
            "threadId": thread_id,
            "input": [{"type":"text", "text":"Reply with exactly RECODEX_OK. Do not use any tools."}]
        })).await.unwrap();
        let mut answer = String::new();
        tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = events.recv().await {
                if event["params"]["threadId"] != thread_id {
                    continue;
                }
                if event["method"] == "item/completed"
                    && event["params"]["item"]["type"] == "agentMessage"
                {
                    answer.push_str(event["params"]["item"]["text"].as_str().unwrap_or_default());
                }
                if event["method"] == "turn/completed" {
                    assert_eq!(
                        event["params"]["turn"]["status"], "completed",
                        "{}",
                        event["params"]["turn"]["error"]
                    );
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            answer.contains("RECODEX_OK"),
            "No expected assistant response received"
        );
    }
}

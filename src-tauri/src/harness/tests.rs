//! End-to-end harness tests through `Host`, with a scripted model and a mock HTTP endpoint.
use super::{
    ledger::{kind, Ledger},
    model::{Delta, ModelClient, ModelRequest, ModelResponse, OpenAiCompatible},
    testing::{reply, tool_call, Scripted, Step},
    Harness,
};
use crate::{host::Host, workspace};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::broadcast,
};

struct Setup {
    host: Arc<Host>,
    root: String,
    _directory: tempfile::TempDir,
}

fn setup(client: Arc<dyn ModelClient>) -> Setup {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "remember the milk").unwrap();
    let root = std::fs::canonicalize(root)
        .unwrap()
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_owned();
    let host = Host::new_with_harness(directory.path().join("data"), |path| {
        Harness::with_client(path, client, Some(8))
    })
    .unwrap();
    workspace::project_save(&host.directory, &host.workspace, root.clone(), None).unwrap();
    Setup {
        host,
        root,
        _directory: directory,
    }
}

async fn next(events: &mut broadcast::Receiver<Value>, method: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let envelope = events.recv().await.unwrap();
            if envelope["event"]["method"] == method {
                return envelope["event"].clone();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {method}"))
}

async fn start(setup: &Setup) -> String {
    let started = setup
        .host
        .request(
            false,
            "thread/start",
            json!({"cwd":setup.root,"model":"recodex/test-model"}),
        )
        .await
        .unwrap();
    assert_eq!(started["thread"]["provider"], "recodex");
    started["thread"]["id"].as_str().unwrap().to_owned()
}

async fn prompt(setup: &Setup, id: &str, text: &str) -> Value {
    setup
        .host
        .request(
            false,
            "turn/start",
            json!({"threadId":id,"input":[{"type":"text","text":text}]}),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn tools_run_after_approval_and_history_is_projected_from_the_ledger() {
    let model = Scripted::new(vec![
        reply(
            "",
            vec![
                tool_call("read", "read_file", json!({"path":"notes.txt"})),
                tool_call(
                    "write",
                    "write_file",
                    json!({"path":"out/todo.txt","content":"buy milk\n"}),
                ),
                tool_call("escape", "read_file", json!({"path":"../secret"})),
            ],
        ),
        reply("Saved the todo.", vec![]),
    ]);
    let setup = setup(model.clone());
    let models = setup
        .host
        .request(false, "model/list", json!({}))
        .await
        .unwrap();
    assert!(models["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == "recodex/test-model"));
    let mut events = setup.host.events.subscribe();
    let id = start(&setup).await;
    let turn = prompt(&setup, &id, "Write a todo").await;
    let run = turn["turn"]["id"].as_str().unwrap().to_owned();
    assert!(
        setup
            .host
            .request(
                false,
                "turn/start",
                json!({"threadId":id,"input":[{"type":"text","text":"again"}]})
            )
            .await
            .is_err(),
        "a second turn in a running chat is rejected"
    );

    let approval = next(&mut events, "item/fileChange/requestApproval").await;
    assert_eq!(approval["params"]["turnId"], run);
    assert!(!Path::new(&setup.root).join("out/todo.txt").exists());
    let snapshot = setup.host.snapshot(false);
    assert_eq!(snapshot["approvals"].as_array().unwrap().len(), 1);
    setup
        .host
        .request(
            false,
            "approval/respond",
            json!({"id":approval["id"],"result":{"decision":"accept"}}),
        )
        .await
        .unwrap();
    assert!(
        setup
            .host
            .request(
                false,
                "approval/respond",
                json!({"id":approval["id"],"result":{"decision":"accept"}})
            )
            .await
            .is_err(),
        "an approval accepts one answer"
    );
    let completed = next(&mut events, "turn/completed").await;
    assert_eq!(completed["params"]["turn"]["status"], "completed");
    assert_eq!(
        std::fs::read_to_string(Path::new(&setup.root).join("out/todo.txt")).unwrap(),
        "buy milk\n"
    );

    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let tool_results: Vec<_> = requests[1]
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(tool_results[0], "remember the milk");
    assert!(tool_results[1].starts_with("Created"));
    assert!(tool_results[2].contains("outside this project"));

    let read = setup
        .host
        .request(false, "thread/resume", json!({"threadId":id}))
        .await
        .unwrap();
    let turn = &read["thread"]["turns"][0];
    assert_eq!(turn["status"], "completed");
    let items: Vec<_> = turn["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            format!(
                "{}:{}",
                item["type"].as_str().unwrap(),
                item["status"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            "userMessage:",
            "mcpToolCall:completed",
            "fileChange:completed",
            "mcpToolCall:failed",
            "agentMessage:"
        ]
    );
    let list = setup
        .host
        .request(false, "thread/list", json!({"cwd":setup.root}))
        .await
        .unwrap();
    assert_eq!(list["data"][0]["preview"], "Write a todo");
}

#[tokio::test]
async fn declines_reach_the_model_and_interrupts_stop_a_pending_request() {
    let model = Scripted::new(vec![
        reply(
            "",
            vec![tool_call(
                "cmd",
                "run_command",
                json!({"command":"touch ran"}),
            )],
        ),
        reply("Understood.", vec![]),
        Step::Hang,
    ]);
    let setup = setup(model.clone());
    let mut events = setup.host.events.subscribe();
    let id = start(&setup).await;
    prompt(&setup, &id, "Run something").await;
    let approval = next(&mut events, "item/commandExecution/requestApproval").await;
    assert_eq!(approval["params"]["command"], "touch ran");
    setup
        .host
        .request(
            false,
            "approval/respond",
            json!({"id":approval["id"],"result":{"decision":"decline"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut events, "turn/completed").await["params"]["turn"]["status"],
        "completed"
    );
    assert!(!Path::new(&setup.root).join("ran").exists());
    let declined = model.requests.lock().unwrap()[1]
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .clone();
    assert_eq!(declined, "The user declined this action.");

    let turn = prompt(&setup, &id, "Wait forever").await;
    let run = turn["turn"]["id"].as_str().unwrap().to_owned();
    // Interrupt while the model request is in flight.
    tokio::time::timeout(Duration::from_secs(10), async {
        while model.requests.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    setup
        .host
        .request(false, "turn/interrupt", json!({"threadId":id,"turnId":run}))
        .await
        .unwrap();
    let completed = next(&mut events, "turn/completed").await;
    assert_eq!(completed["params"]["turn"]["status"], "interrupted");
    assert!(setup.host.snapshot(false)["activeTurns"]
        .as_object()
        .unwrap()
        .is_empty());
    // The second prompt sees the first exchange, including the declined call.
    let third = model.requests.lock().unwrap()[2].clone();
    assert_eq!(
        third.iter().filter(|m| m["role"] == "user").count(),
        2,
        "{third:?}"
    );
    setup
        .host
        .request(false, "thread/delete", json!({"threadId":id}))
        .await
        .unwrap();
    assert!(setup
        .host
        .request(false, "thread/read", json!({"threadId":id}))
        .await
        .is_err());
}

#[tokio::test]
async fn recovery_closes_runs_left_open_by_a_previous_process() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("harness.sqlite3");
    {
        let ledger = Ledger::open(&path).unwrap();
        ledger
            .create_session("rx-old", "/project", Some("recodex/m"), None, json!({}))
            .unwrap();
        for (kind, payload) in [
            (kind::INPUT_ACCEPTED, json!({"inputId":"i","text":"go"})),
            (kind::RUN_STARTED, json!({"runId":"r","inputId":"i"})),
            (
                kind::MESSAGE_ASSISTANT,
                json!({"runId":"r","itemId":"m","text":"","toolCalls":[
                    {"id":"c","name":"run_command","arguments":"{\"command\":\"deploy\"}"}
                ]}),
            ),
            (
                kind::TOOL_STARTED,
                json!({"runId":"r","callId":"c","item":{"id":"t","type":"commandExecution","status":"inProgress"}}),
            ),
            (kind::TOOL_DISPATCHED, json!({"runId":"r","callId":"c"})),
        ] {
            ledger.append("rx-old", None, kind, payload).unwrap();
        }
    }
    let harness = Harness::with_client(&path, Scripted::new(vec![]), None);
    let thread = harness.read_thread("rx-old").unwrap();
    let turn = &thread["thread"]["turns"][0];
    assert_eq!(turn["status"], "interrupted");
    assert_eq!(turn["items"][1]["status"], "failed");
    let events = harness.ledger().unwrap().events("rx-old").unwrap();
    let result = events
        .iter()
        .find(|event| event.kind == kind::TOOL_COMPLETED)
        .unwrap();
    assert_eq!(result.payload["status"], "outcomeUnknown");
    let messages = super::context::messages(&events, "/project", u64::MAX);
    assert!(messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains("outcome is unknown"));
    assert!(harness
        .ledger()
        .unwrap()
        .unfinished_runs()
        .unwrap()
        .is_empty());
}

/// Serve one HTTP request with a canned response and return the raw request text.
async fn serve_once(response: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if read == 0 {
                break;
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (address, handle)
}

#[tokio::test]
async fn openai_compatible_adapter_streams_over_http() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"list_directory\",\"arguments\":\"{\\\"path\\\":\\\".\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
        "data: [DONE]\n\n"
    );
    let (address, request) = serve_once(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    let client = OpenAiCompatible::new(address, Some("test-key".into()), None).unwrap();
    let deltas = Mutex::new(Vec::new());
    let response = client
        .stream(
            &ModelRequest {
                model: "m".into(),
                messages: vec![json!({"role":"user","content":"hello"})],
                tools: super::tools::schemas(&super::tools::registry()),
                effort: Some("low".into()),
                cwd: "/p".into(),
                session: "rx-1".into(),
            },
            &|delta| {
                if let Delta::Text(text) = delta {
                    deltas.lock().unwrap().push(text);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(*deltas.lock().unwrap(), ["Hi"]);
    assert_eq!(response.tool_calls[0].name, "list_directory");
    assert_eq!(response.usage.unwrap().total, 5);
    let request = request.await.unwrap();
    assert!(request.starts_with("POST /v1/chat/completions"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-key"));
    let json: Value =
        serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(json["stream"], true);
    assert_eq!(json["reasoning_effort"], "low");
    assert_eq!(json["tools"].as_array().unwrap().len(), 5);

    let body = r#"{"error":{"message":"bad api key"}}"#;
    let (address, _) = serve_once(format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    let client = OpenAiCompatible::new(address, None, None).unwrap();
    let error = client
        .stream(
            &ModelRequest {
                model: "m".into(),
                messages: vec![],
                tools: vec![],
                effort: None,
                cwd: "/p".into(),
                session: "rx-1".into(),
            },
            &|_| {},
        )
        .await
        .unwrap_err();
    assert!(!error.retryable);
    assert!(error.message.contains("bad api key"), "{}", error.message);
}

/// A Command Code API double: `/alpha/whoami` accepts only `cc_key`, and `/provider/v1/models`
/// lists one Chat Completions model and one Anthropic-only model.
async fn command_code_api() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let line = request.lines().next().unwrap_or_default().to_owned();
            let authorized = request
                .to_ascii_lowercase()
                .contains("authorization: bearer cc_key");
            log.lock()
                .unwrap()
                .push(format!("{line} authorized={authorized}"));
            let (status, body) = match (line.split(' ').nth(1).unwrap_or_default(), authorized) {
                (_, false) => ("401 Unauthorized", r#"{"error":"unauthorized"}"#.to_owned()),
                ("/alpha/whoami", true) => ("200 OK", r#"{"user":{"userName":"Ada"}}"#.to_owned()),
                ("/provider/v1/models", true) => (
                    "200 OK",
                    json!({"data":[
                        {"id":"deepseek/deepseek-v4-flash","supported_endpoints":["/chat/completions","/responses"]},
                        {"id":"claude-sonnet-4-6","supported_endpoints":["/messages"]}
                    ]})
                    .to_string(),
                ),
                _ => ("404 Not Found", "{}".to_owned()),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (address, seen)
}

/// Complete the Studio's side of a browser sign-in for `auth_url`.
async fn studio_redirect(auth_url: &str, api_key: &str) {
    let query = super::command_code::parse_query(auth_url.split_once('?').unwrap().1);
    let port: u16 = query["callback"]
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/callback")
        .parse()
        .unwrap();
    let send = |request: String| async move {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };
    let form = format!(
        "apiKey={api_key}&state={}&userId=u1&userName=&keyName=recodex",
        query["state"]
    );
    let posted = send(format!(
        "POST /callback HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    ))
    .await;
    assert!(
        posted.starts_with("HTTP/1.1 400"),
        "an empty userName is rejected"
    );
    let form = form.replace("userName=", "userName=Ada");
    let posted = send(format!(
        "POST /callback HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{form}",
        form.len()
    ))
    .await;
    let location = posted
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .unwrap()
        .to_owned();
    assert!(send(format!("GET {location} HTTP/1.1\r\n\r\n"))
        .await
        .starts_with("HTTP/1.1 200"));
}

#[tokio::test]
async fn command_code_sign_in_switches_the_model_connection() {
    let (api, seen) = command_code_api().await;
    let directory = tempfile::tempdir().unwrap();
    let store = super::command_code::Store {
        path: directory.path().join("command-code-auth.json"),
        cli_path: None,
        env_key: None,
    };
    let harness = Harness::with_account(
        &directory.path().join("harness.sqlite3"),
        super::account::Account::for_test(
            super::HarnessConfig::default(),
            store,
            "https://studio.test",
            &api,
        ),
    );
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    harness.set_emit(Arc::new(move |event| {
        let _ = sender.send(event);
    }));
    assert!(!harness.available());
    assert_eq!(harness.account()["signedIn"], false);

    let url = harness.login_start().await.unwrap();
    assert!(url.starts_with("https://studio.test/studio/auth/cli?"));
    assert_eq!(harness.account()["loginPending"], true);
    studio_redirect(&url, "bad_key").await;
    let failed = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed["method"], "account/login/completed");
    assert!(failed["params"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("rejected"));
    assert!(!harness.available(), "an invalid key is not stored");

    let url = harness.login_start().await.unwrap();
    studio_redirect(&url, "cc_key").await;
    let updated = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated["method"], "account/updated");
    assert!(harness.available());
    assert_eq!(harness.connection_label(), "Command Code");
    let account = harness.account();
    assert_eq!(account["userName"], "Ada");
    assert_eq!(account["canSignOut"], true);
    assert_eq!(account["loginPending"], false);
    assert!(!account.to_string().contains("cc_key"));
    let models = harness.models().await.unwrap();
    let ids: Vec<_> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "recodex/claude-sonnet-4-6",
            "recodex/deepseek/deepseek-v4-flash"
        ]
    );
    assert_eq!(models[0]["providerLabel"], "Command Code");
    assert!(seen
        .lock()
        .unwrap()
        .contains(&"GET /provider/v1/models HTTP/1.1 authorized=true".to_owned()));

    harness.logout().unwrap();
    assert!(!harness.available());
    assert_eq!(harness.account()["signedIn"], false);
}

fn command_code_request(model: &str) -> ModelRequest {
    ModelRequest {
        model: model.into(),
        messages: vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":"hello"}),
        ],
        tools: super::tools::schemas(&super::tools::registry()),
        effort: None,
        cwd: "/work/project".into(),
        session: format!("rx-{}", uuid::Uuid::nil()),
    }
}

#[tokio::test]
async fn command_code_routes_models_and_falls_back_to_generate() {
    let catalog = json!({"data":[
        {"id":"claude-sonnet-4-6","name":"Claude","context_length":200000,"supported_endpoints":["/messages"]},
        {"id":"gpt-5.6","name":"GPT","context_length":400000,"supported_endpoints":["/chat/completions","/responses"]},
        {"id":"deepseek/deepseek-v4-flash","name":"DeepSeek","context_length":1000000,"supported_endpoints":["/chat/completions"]}
    ]})
    .to_string();
    let (api, seen) = super::testing::mock_http(move |_, path| match path {
        "/provider/v1/models" => ("200 OK", "application/json", catalog.clone()),
        "/provider/v1/messages" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"from claude\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            )
            .into(),
        ),
        "/provider/v1/responses" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"from responses\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
            )
            .into(),
        ),
        "/provider/v1/chat/completions" => (
            "403 Forbidden",
            "application/json",
            r#"{"error":{"code":"upgrade_required","message":"Upgrade to use the Provider API"}}"#.into(),
        ),
        "/alpha/generate" => (
            "200 OK",
            "text/event-stream",
            concat!(
                "data: {\"type\":\"text-delta\",\"text\":\"from generate\"}\n\n",
                "data: {\"type\":\"finish\",\"finishReason\":\"stop\",\"totalUsage\":{\"inputTokens\":5,\"outputTokens\":2}}\n\n"
            )
            .into(),
        ),
        _ => ("404 Not Found", "application/json", "{}".into()),
    })
    .await;
    let client =
        super::command_code_client::CommandCodeClient::new(&api, "cc_key".into(), None).unwrap();
    let models = client.models().await.unwrap();
    assert_eq!(models.len(), 3);
    let text = |response: ModelResponse| response.text;

    let claude = client
        .stream(&command_code_request("claude-sonnet-4-6"), &|_| {})
        .await
        .unwrap();
    assert_eq!(text(claude), "from claude");
    let responses = client
        .stream(&command_code_request("gpt-5.6"), &|_| {})
        .await
        .unwrap();
    assert_eq!(text(responses), "from responses");
    assert!(!client.uses_generate());

    let streamed = Mutex::new(String::new());
    let fallback = client
        .stream(
            &command_code_request("deepseek/deepseek-v4-flash"),
            &|delta| {
                if let Delta::Text(text) = delta {
                    streamed.lock().unwrap().push_str(&text);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(fallback.usage.unwrap().total, 7);
    assert_eq!(text(fallback), "from generate");
    assert_eq!(*streamed.lock().unwrap(), "from generate");
    assert!(client.uses_generate());
    // Later requests go straight to generate, whatever the model's route.
    let again = client
        .stream(&command_code_request("claude-sonnet-4-6"), &|_| {})
        .await
        .unwrap();
    assert_eq!(text(again), "from generate");

    let seen = seen.lock().unwrap().clone();
    let paths: Vec<_> = seen.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/provider/v1/models",
            "/provider/v1/messages",
            "/provider/v1/responses",
            "/provider/v1/chat/completions",
            "/alpha/generate",
            "/alpha/generate"
        ]
    );
    assert!(seen
        .iter()
        .all(|r| r.headers.contains("authorization: bearer cc_key")));
    assert_eq!(seen[0].method, "GET");
    assert!(seen[1..].iter().all(|r| r.method == "POST"));
    assert!(seen[1].headers.contains("anthropic-version: 2023-06-01"));
    let messages: Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!(messages["system"], "rules");
    assert_eq!(messages["tools"].as_array().unwrap().len(), 5);
    let responses: Value = serde_json::from_str(&seen[2].body).unwrap();
    assert_eq!(responses["instructions"], "rules");
    assert_eq!(responses["store"], false);
    let generate: Value = serde_json::from_str(&seen[4].body).unwrap();
    assert_eq!(generate["params"]["system"], "rules");
    assert_eq!(generate["params"]["messages"][0]["content"], "hello");
    assert_eq!(generate["config"]["workingDir"], "/work/project");
    assert_eq!(generate["threadId"], uuid::Uuid::nil().to_string());
    assert!(seen[4].headers.contains("x-project-slug: work-project"));
    assert!(seen[4].headers.contains("x-command-code-version:"));
}

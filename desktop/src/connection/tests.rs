use super::*;
use serde_json::json;
use std::{sync::Mutex, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
			let authorized = request.to_ascii_lowercase().contains("authorization: bearer cc_key");
			log.lock().unwrap().push(format!("{line} authorized={authorized}"));
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
	let query = command_code::parse_query(auth_url.split_once('?').unwrap().1);
	let port: u16 = query["callback"]
		.trim_start_matches("http://127.0.0.1:")
		.trim_end_matches("/callback")
		.parse()
		.unwrap();
	let send = |request: String| async move {
		let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
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
	assert!(posted.starts_with("HTTP/1.1 400"), "an empty userName is rejected");
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
	let store = Store {
		path: directory.path().join("command-code-auth.json"),
		cli_path: None,
		env_key: None,
	};
	let harness = Harness::new(&directory.path().join("harness.sqlite3"), None);
	let connection = Connection::for_test(harness.clone(), Config::default(), store, "https://studio.test", &api);
	let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
	harness.set_emit(Arc::new(move |event| {
		let _ = sender.send(event);
	}));
	assert!(!harness.available());
	assert_eq!(connection.account()["signedIn"], false);

	let url = connection.login_start().await.unwrap();
	assert!(url.starts_with("https://studio.test/studio/auth/cli?"));
	assert_eq!(connection.account()["loginPending"], true);
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

	let url = connection.login_start().await.unwrap();
	studio_redirect(&url, "cc_key").await;
	let updated = tokio::time::timeout(Duration::from_secs(5), events.recv())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(updated["method"], "account/updated");
	assert!(harness.available());
	assert_eq!(harness.connection_label(), "Command Code");
	let account = connection.account();
	assert_eq!(account["userName"], "Ada");
	assert_eq!(account["canSignOut"], true);
	assert_eq!(account["loginPending"], false);
	assert!(!account.to_string().contains("cc_key"));
	let models = harness.models().await.unwrap();
	let ids: Vec<_> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
	assert_eq!(ids, ["recodex/claude-sonnet-4-6", "recodex/deepseek/deepseek-v4-flash"]);
	assert_eq!(models[0]["providerLabel"], "Command Code");
	assert!(seen
		.lock()
		.unwrap()
		.contains(&"GET /provider/v1/models HTTP/1.1 authorized=true".to_owned()));

	connection.logout().unwrap();
	assert!(!harness.available());
	assert_eq!(connection.account()["signedIn"], false);
}

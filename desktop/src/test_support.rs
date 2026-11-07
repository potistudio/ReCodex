//! Test helpers shared by the harness, provider, and connection tests.
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Five function-tool schemas in the canonical Chat Completions shape.
pub(crate) fn tool_schemas() -> Vec<Value> {
	["read_file", "write_file", "list_directory", "search", "run_command"]
		.iter()
		.map(|name| {
			json!({"type":"function","function":{"name":name,"description":"A test tool","parameters":{"type":"object"}}})
		})
		.collect()
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

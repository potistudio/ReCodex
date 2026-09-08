mod server;
use serde_json::{json, Value};
use tauri::{Emitter, Manager, State};
use tokio::sync::RwLock;
#[derive(Default)]
struct Connection(RwLock<Option<server::Server>>);
#[tauri::command]
async fn server_connect(app: tauri::AppHandle, state: State<'_, Connection>) -> Result<(), String> {
    let mut connection = state.0.write().await;
    if connection
        .as_mut()
        .is_some_and(|server| server.is_running())
    {
        return Ok(());
    }
    *connection = None;
    *connection = Some(
        server::Server::start(move |event| {
            let _ = app.emit("codex-event", event);
        })
        .await?,
    );
    Ok(())
}
#[tauri::command]
async fn server_request(
    state: State<'_, Connection>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let connection = state.0.read().await;
    connection
        .as_ref()
        .ok_or("Codex is not connected")?
        .request(&method, params)
        .await
}
#[tauri::command]
async fn server_respond(
    state: State<'_, Connection>,
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
) -> Result<(), String> {
    let connection = state.0.read().await;
    let response = if let Some(error) = error {
        json!({"id":id,"error":error})
    } else {
        json!({"id":id,"result":result})
    };
    connection
        .as_ref()
        .ok_or("Codex is not connected")?
        .write(response)
        .await
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .manage(Connection::default())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            server_connect,
            server_request,
            server_respond,
        ])
        .build(tauri::generate_context!())
        .expect("Could not start ReCodex");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            tauri::async_runtime::block_on(async {
                *app.state::<Connection>().0.write().await = None;
            });
        }
    });
}

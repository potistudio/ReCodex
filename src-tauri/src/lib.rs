mod server;
mod workspace;
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
        .manage(workspace::Workspace::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            server_connect,
            server_request,
            server_respond,
            workspace::projects_load,
            workspace::project_save,
            workspace::project_relocate,
            workspace::project_remove,
            workspace::files_list,
            workspace::file_read,
            workspace::file_save
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

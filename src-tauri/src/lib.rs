mod host;
mod lan;
mod server;
mod workspace;
use host::{Host, LOCAL};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock};

struct Connections {
    local: Arc<Host>,
    listener: Mutex<Option<Arc<lan::Listener>>>,
    remotes: RwLock<HashMap<String, Arc<lan::Remote>>>,
}
impl Connections {
    async fn request(
        &self,
        id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match id.unwrap_or(LOCAL) {
            LOCAL => self.local.request(LOCAL, false, method, params).await,
            id => {
                let remote = self
                    .remotes
                    .read()
                    .await
                    .get(id)
                    .cloned()
                    .ok_or("Unknown host")?;
                remote.request(method, params).await
            }
        }
    }
}
fn emit_remote(app: tauri::AppHandle, id: String) -> impl Fn(Value) + Send + 'static {
    move |envelope| {
        let _ = app.emit(
            "codex-event",
            json!({"hostId":id,"event":envelope["event"],"epoch":envelope["epoch"],"sequence":envelope["sequence"]}),
        );
    }
}
#[tauri::command]
async fn server_connect(
    app: tauri::AppHandle,
    state: State<'_, Connections>,
    host_id: Option<String>,
) -> Result<(), String> {
    match host_id.as_deref().unwrap_or(LOCAL) {
        LOCAL => state.local.connect().await,
        id => {
            let remote = state
                .remotes
                .read()
                .await
                .get(id)
                .cloned()
                .ok_or("Unknown host")?;
            remote.connect(emit_remote(app, id.into())).await?;
            remote.request("host/connect", json!({})).await?;
            Ok(())
        }
    }
}
#[tauri::command]
async fn server_request(
    state: State<'_, Connections>,
    host_id: Option<String>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    state.request(host_id.as_deref(), &method, params).await
}
#[tauri::command]
async fn server_respond(
    state: State<'_, Connections>,
    host_id: Option<String>,
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
) -> Result<(), String> {
    state
        .request(
            host_id.as_deref(),
            "approval/respond",
            json!({"id":id,"result":result,"error":error}),
        )
        .await?;
    Ok(())
}
#[tauri::command]
async fn projects_load(
    state: State<'_, Connections>,
    host_id: Option<String>,
) -> Result<Value, String> {
    state
        .request(host_id.as_deref(), "projects/list", json!({}))
        .await
}
#[tauri::command]
async fn files_list(
    state: State<'_, Connections>,
    host_id: Option<String>,
    root: String,
    path: String,
) -> Result<Value, String> {
    state
        .request(
            host_id.as_deref(),
            "files/list",
            json!({"root":root,"path":path}),
        )
        .await
}
#[tauri::command]
async fn file_read(
    state: State<'_, Connections>,
    host_id: Option<String>,
    root: String,
    path: String,
) -> Result<Value, String> {
    state
        .request(
            host_id.as_deref(),
            "files/read",
            json!({"root":root,"path":path}),
        )
        .await
}
#[tauri::command]
async fn file_save(
    state: State<'_, Connections>,
    host_id: Option<String>,
    root: String,
    path: String,
    content: String,
    expected: String,
) -> Result<Value, String> {
    state
        .request(
            host_id.as_deref(),
            "files/save",
            json!({"root":root,"path":path,"content":content,"expected":expected}),
        )
        .await
}
#[tauri::command]
fn project_save(
    state: State<'_, Connections>,
    path: String,
    name: Option<String>,
) -> Result<Vec<workspace::Project>, String> {
    workspace::project_save(&state.local.directory, &state.local.workspace, path, name)
}
#[tauri::command]
fn project_remove(
    state: State<'_, Connections>,
    path: String,
) -> Result<Vec<workspace::Project>, String> {
    workspace::project_remove(&state.local.directory, &state.local.workspace, path)
}
#[tauri::command]
fn project_relocate(
    state: State<'_, Connections>,
    source_path: String,
    destination_path: String,
) -> Result<workspace::Project, String> {
    workspace::project_relocate(
        &state.local.directory,
        &state.local.workspace,
        source_path,
        destination_path,
    )
}

#[tauri::command]
async fn host_status(state: State<'_, Connections>) -> Result<Value, String> {
    let listener = state.listener.lock().await;
    let mut status = listener.as_ref().map(|l| l.status()).unwrap_or(Value::Null);
    if status.is_object() {
        status["sharedProjects"] = json!(*state.local.shared.lock().unwrap());
    }
    Ok(status)
}
#[tauri::command]
async fn host_start(
    state: State<'_, Connections>,
    address: String,
    name: String,
    projects: Vec<String>,
) -> Result<Value, String> {
    let mut listener = state.listener.lock().await;
    if listener.is_some() {
        return Err("Hosting is already enabled. Stop it before changing its settings.".into());
    }
    state.local.connect().await?;
    let started = lan::listen(state.local.clone(), &address, name, projects).await?;
    let status = started.status();
    *listener = Some(started);
    Ok(status)
}
#[tauri::command]
async fn host_stop(state: State<'_, Connections>) -> Result<(), String> {
    if let Some(listener) = state.listener.lock().await.take() {
        listener.shutdown().await;
    }
    state.local.shared.lock().unwrap().clear();
    Ok(())
}
#[tauri::command]
async fn host_invite(state: State<'_, Connections>) -> Result<String, String> {
    let listener = state.listener.lock().await;
    serde_json::to_string(&listener.as_ref().ok_or("Enable hosting first")?.invite())
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn host_revoke(state: State<'_, Connections>, device_id: String) -> Result<(), String> {
    state
        .listener
        .lock()
        .await
        .as_ref()
        .ok_or("Hosting is disabled")?
        .revoke(&device_id);
    Ok(())
}
#[tauri::command]
async fn host_pair(
    app: tauri::AppHandle,
    state: State<'_, Connections>,
    address: String,
    invitation: String,
) -> Result<Value, String> {
    if address.contains('/') || address.len() > 260 {
        return Err(
            "Enter a host name or IP address and port, for example 192.168.1.20:43177".into(),
        );
    }
    let invitation: lan::Invitation =
        serde_json::from_str(&invitation).map_err(|_| "Invalid connection invitation")?;
    let remote = lan::Remote::new(address, invitation);
    if state.remotes.read().await.contains_key(&remote.id) {
        return Err(
            "This host is already registered in this session. Select it and reconnect.".into(),
        );
    }
    remote.connect(emit_remote(app, remote.id.clone())).await?;
    let result = json!({"id":remote.id,"name":remote.name});
    state
        .remotes
        .write()
        .await
        .insert(remote.id.clone(), remote);
    Ok(result)
}
#[tauri::command]
async fn host_forget(state: State<'_, Connections>, host_id: String) -> Result<(), String> {
    let remote = state.remotes.write().await.remove(&host_id);
    if let Some(remote) = remote {
        remote.disconnect().await;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let local = Host::new(app.path().app_data_dir()?).map_err(std::io::Error::other)?;
            let mut events = local.events.subscribe(); let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop { match events.recv().await {
                    Ok(envelope) => { let _ = handle.emit("codex-event", json!({"hostId":LOCAL,"event":envelope["event"],"epoch":envelope["epoch"],"sequence":envelope["sequence"]})); }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => { let _ = handle.emit("codex-event", json!({"hostId":LOCAL,"event":{"method":"recodex/disconnected","params":{}}})); }
                    Err(_) => break,
                } }
            });
            app.manage(Connections { local, listener:Mutex::new(None), remotes:RwLock::new(HashMap::new()) });
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![server_connect, server_request, server_respond, projects_load, project_save, project_relocate, project_remove, files_list, file_read, file_save, host_status, host_start, host_stop, host_invite, host_revoke, host_pair, host_forget])
        .build(tauri::generate_context!()).expect("Could not start ReCodex");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            tauri::async_runtime::block_on(async {
                let state = app.state::<Connections>();
                if let Some(listener) = state.listener.lock().await.take() {
                    listener.shutdown().await;
                }
                let remotes: Vec<_> = state
                    .remotes
                    .write()
                    .await
                    .drain()
                    .map(|(_, remote)| remote)
                    .collect();
                for remote in remotes {
                    remote.disconnect().await;
                }
                state.local.shutdown().await;
            });
        }
    });
}

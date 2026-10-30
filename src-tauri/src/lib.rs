mod discovery;
mod host;
mod lan;
mod lan_store;
mod server;
mod workspace;
use host::{Host, LOCAL};
use lan_store::Hosting;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tauri::{Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex, RwLock};

struct Connections {
    local: Arc<Host>,
    listener: Mutex<Option<Arc<lan::Listener>>>,
    remotes: RwLock<HashMap<String, Arc<lan::Remote>>>,
    discovery: Arc<discovery::Discovery>,
    hosting_error: std::sync::Mutex<Option<String>>,
}
impl Connections {
    async fn start_host(&self, config: Hosting, persist: bool) -> Result<Value, String> {
        let mut listener = self.listener.lock().await;
        if listener.is_some() {
            return Err("Hosting is already enabled. Stop it before changing its settings.".into());
        }
        self.local.connect().await?;
        let started = lan::listen(
            self.local.clone(),
            &config.address,
            config.name.clone(),
            config.projects.clone(),
        )
        .await?;
        if persist {
            if let Err(e) = self.local.lan.update(|s| s.hosting = Some(config)) {
                started.shutdown().await;
                self.local.shared.lock().unwrap().clear();
                return Err(e);
            }
        }
        *self.hosting_error.lock().unwrap() = self
            .discovery
            .advertise(&started.id, &started.name, &started.address)
            .err();
        let status = started.status();
        *listener = Some(started);
        Ok(status)
    }
    fn endpoints(&self, id: &str, fallback: Vec<String>) -> Vec<String> {
        let mut endpoints = self.discovery.addresses(id);
        for address in fallback {
            if !endpoints.contains(&address) {
                endpoints.push(address);
            }
        }
        endpoints.truncate(8);
        endpoints
    }

    async fn remember_current(&self, remote: &Arc<lan::Remote>) -> Result<(), String> {
        let remotes = self.remotes.read().await;
        if remotes
            .get(&remote.id)
            .is_some_and(|current| Arc::ptr_eq(current, remote))
        {
            let profile = remote.profile();
            if self
                .local
                .lan
                .snapshot()
                .remotes
                .iter()
                .find(|p| p.id == remote.id)
                != Some(&profile)
            {
                self.local.lan.remember(profile)?;
            }
        }
        Ok(())
    }
    async fn register(&self, remote: Arc<lan::Remote>) -> Result<(), String> {
        let previous = {
            let mut remotes = self.remotes.write().await;
            self.local.lan.remember(remote.profile())?;
            remotes.insert(remote.id.clone(), remote)
        };
        if let Some(previous) = previous {
            previous.disconnect().await;
        }
        Ok(())
    }
    async fn forget(&self, id: &str) -> Result<(), String> {
        let remote = {
            let mut remotes = self.remotes.write().await;
            self.local
                .lan
                .update(|s| s.remotes.retain(|p| p.id != id))?;
            remotes.remove(id)
        };
        if let Some(remote) = remote {
            remote.disconnect().await;
        }
        Ok(())
    }
    async fn request(
        &self,
        id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match id.unwrap_or(LOCAL) {
            LOCAL => self.local.request(false, method, params).await,
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
    state: State<'_, Arc<Connections>>,
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
            remote.update_endpoints(state.endpoints(id, remote.profile().endpoints));
            remote.connect(emit_remote(app, id.into())).await?;
            remote.request("host/connect", json!({})).await?;
            state.remember_current(&remote).await?;
            Ok(())
        }
    }
}
#[tauri::command]
async fn server_request(
    state: State<'_, Arc<Connections>>,
    host_id: Option<String>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    state.request(host_id.as_deref(), &method, params).await
}
#[tauri::command]
async fn server_respond(
    state: State<'_, Arc<Connections>>,
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
    state: State<'_, Arc<Connections>>,
    host_id: Option<String>,
) -> Result<Value, String> {
    state
        .request(host_id.as_deref(), "projects/list", json!({}))
        .await
}
#[tauri::command]
async fn files_list(
    state: State<'_, Arc<Connections>>,
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
    state: State<'_, Arc<Connections>>,
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
    state: State<'_, Arc<Connections>>,
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
    state: State<'_, Arc<Connections>>,
    path: String,
    name: Option<String>,
) -> Result<Vec<workspace::Project>, String> {
    workspace::project_save(&state.local.directory, &state.local.workspace, path, name)
}
#[tauri::command]
fn project_remove(
    state: State<'_, Arc<Connections>>,
    path: String,
) -> Result<Vec<workspace::Project>, String> {
    workspace::project_remove(&state.local.directory, &state.local.workspace, path)
}
#[tauri::command]
fn project_relocate(
    state: State<'_, Arc<Connections>>,
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
async fn host_status(state: State<'_, Arc<Connections>>) -> Result<Value, String> {
    let listener = state.listener.lock().await;
    let mut status = listener.as_ref().map(|l| l.status()).unwrap_or(Value::Null);
    if status.is_object() {
        status["sharedProjects"] = json!(*state.local.shared.lock().unwrap());
    }
    Ok(status)
}
#[tauri::command]
async fn host_start(
    state: State<'_, Arc<Connections>>,
    address: String,
    name: String,
    projects: Vec<String>,
) -> Result<Value, String> {
    state
        .start_host(
            Hosting {
                address,
                name,
                projects,
            },
            true,
        )
        .await
}
#[tauri::command]
async fn host_stop(state: State<'_, Arc<Connections>>) -> Result<(), String> {
    let mut listener = state.listener.lock().await;
    state.local.lan.update(|s| s.hosting = None)?;
    state.discovery.unadvertise();
    *state.hosting_error.lock().unwrap() = None;
    if let Some(listener) = listener.take() {
        listener.shutdown().await;
    }
    state.local.shared.lock().unwrap().clear();
    Ok(())
}
#[tauri::command]
async fn host_invite(state: State<'_, Arc<Connections>>) -> Result<String, String> {
    let listener = state.listener.lock().await;
    serde_json::to_string(&listener.as_ref().ok_or("Enable hosting first")?.invite())
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn host_revoke(state: State<'_, Arc<Connections>>, device_id: String) -> Result<(), String> {
    state
        .listener
        .lock()
        .await
        .as_ref()
        .ok_or("Hosting is disabled")?
        .revoke(&device_id)?;
    Ok(())
}
#[tauri::command]
async fn host_pair(
    app: tauri::AppHandle,
    state: State<'_, Arc<Connections>>,
    address: Option<String>,
    invitation: String,
) -> Result<Value, String> {
    let address = address.unwrap_or_default().trim().to_owned();
    if address.contains('/') || address.len() > 260 {
        return Err("Invalid manual address".into());
    }
    let invitation: lan::Invitation =
        serde_json::from_str(&invitation).map_err(|_| "Invalid connection invitation")?;
    if invitation.host_id() == lan::local_id(&state.local.directory)? {
        return Err("This is your own PC. Choose This PC in Active host.".into());
    }
    let endpoints = state.endpoints(
        invitation.host_id(),
        if address.is_empty() {
            Vec::new()
        } else {
            vec![address]
        },
    );
    if endpoints.is_empty() {
        return Err("This PC has not appeared on the LAN yet. Enable hosting there, or use a manual address.".into());
    }
    let remote = lan::Remote::new(String::new(), invitation);
    remote.update_endpoints(endpoints);
    remote
        .connect(emit_remote(app.clone(), remote.id.clone()))
        .await?;
    let result = json!({"id":remote.id,"name":remote.name});
    state.register(remote).await?;
    let _ = app.emit("hosts-changed", json!({}));
    Ok(result)
}
#[tauri::command]
async fn host_forget(state: State<'_, Arc<Connections>>, host_id: String) -> Result<(), String> {
    state.forget(&host_id).await
}

#[tauri::command]
async fn hosts_list(state: State<'_, Arc<Connections>>) -> Result<Value, String> {
    let registered: Vec<_> = state
        .remotes
        .read()
        .await
        .values()
        .map(|remote| json!({"id":remote.id,"name":remote.name}))
        .collect();
    Ok(
        json!({"registered":registered,"discovered":state.discovery.snapshot(),"discoveryError":state.discovery.error(),"hostingError":*state.hosting_error.lock().unwrap()}),
    )
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
            let own_id = lan::local_id(&local.directory).map_err(std::io::Error::other)?;
            let discovery = tauri::async_runtime::block_on(async { discovery::Discovery::start(own_id) });
            let mut updates = discovery.updates.subscribe(); let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if matches!(updates.recv().await,Err(broadcast::error::RecvError::Closed)) { break; }
                    let _ = handle.emit("hosts-changed",json!({}));
                }
            });
            let saved = local.lan.snapshot();
            let remotes = saved.remotes.into_iter().map(|profile|(profile.id.clone(),lan::Remote::restore(profile))).collect();
            let connections = Arc::new(Connections { local,listener:Mutex::new(None),remotes:RwLock::new(remotes),discovery,hosting_error:std::sync::Mutex::new(None) });
            app.manage(connections.clone());
            if let Some(config) = saved.hosting {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = connections.start_host(config,false).await { *connections.hosting_error.lock().unwrap() = Some(e); }
                    let _ = handle.emit("hosts-changed",json!({}));
                });
            }
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![server_connect, server_request, server_respond, projects_load, project_save, project_relocate, project_remove, files_list, file_read, file_save, host_status, host_start, host_stop, host_invite, host_revoke, host_pair, host_forget, hosts_list])
        .build(tauri::generate_context!()).expect("Could not start ReCodex");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            tauri::async_runtime::block_on(async {
                let state = app.state::<Arc<Connections>>();
                state.discovery.shutdown();
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

#[cfg(test)]
mod connection_tests {
    use super::*;
    #[tokio::test]
    async fn late_reconnect_cannot_undo_forget_or_replace_new_pairing() {
        let fixture = host::Fixture::new().await;
        let connections = Connections {
            local: fixture.host.clone(),
            listener: Mutex::new(None),
            remotes: RwLock::new(HashMap::new()),
            discovery: discovery::Discovery::with_daemon(
                "test".into(),
                Err(mdns_sd::Error::Msg("test".into())),
            ),
            hosting_error: std::sync::Mutex::new(None),
        };
        let profile = lan_store::RemoteProfile {
            id: "peer".into(),
            name: "PC".into(),
            version: 1,
            certificate: vec![],
            token: "old".into(),
            endpoints: vec![],
        };
        let old = lan::Remote::restore(profile.clone());
        connections.register(old.clone()).await.unwrap();
        connections.forget("peer").await.unwrap();
        connections.remember_current(&old).await.unwrap();
        assert!(fixture.host.lan.snapshot().remotes.is_empty());
        let mut profile = profile;
        profile.token = "new".into();
        connections
            .register(lan::Remote::restore(profile))
            .await
            .unwrap();
        connections.remember_current(&old).await.unwrap();
        assert_eq!(fixture.host.lan.snapshot().remotes[0].token, "new");
        fixture.host.shutdown().await;
    }
}

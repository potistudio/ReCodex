//! Durable LAN preferences and trust. This file is never exposed over IPC.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hosting {
    pub address: String,
    pub name: String,
    pub projects: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    pub verifier: [u8; 32],
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProfile {
    pub id: String,
    pub name: String,
    pub version: u64,
    pub certificate: Vec<u8>,
    pub token: String,
    pub endpoints: Vec<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub hosting: Option<Hosting>,
    pub devices: Vec<SavedDevice>,
    pub remotes: Vec<RemoteProfile>,
}
pub struct LanStore {
    path: PathBuf,
    settings: Mutex<Settings>,
}
impl LanStore {
    pub fn load(directory: &Path) -> Result<Arc<Self>, String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let path = directory.join("lan-settings.json");
        let settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Could not read LAN settings: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Arc::new(Self {
            path,
            settings: Mutex::new(settings),
        }))
    }
    pub fn snapshot(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
        let mut current = self.settings.lock().unwrap();
        let mut next = current.clone();
        change(&mut next);
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, &next).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(&self.path).map_err(|e| e.to_string())?;
        *current = next;
        Ok(())
    }
    pub fn remember(&self, remote: RemoteProfile) -> Result<(), String> {
        self.update(|settings| {
            settings.remotes.retain(|p| p.id != remote.id);
            settings.remotes.push(remote);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_and_hosting_survive_restart_and_forgetting_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let store = LanStore::load(directory.path()).unwrap();
        store
            .remember(RemoteProfile {
                id: "peer".into(),
                name: "PC".into(),
                version: 1,
                certificate: vec![1, 2, 3],
                token: "secret".into(),
                endpoints: vec!["192.168.1.20:43177".into()],
            })
            .unwrap();
        store
            .update(|s| {
                s.hosting = Some(Hosting {
                    address: "0.0.0.0:43177".into(),
                    name: "PC".into(),
                    projects: vec!["/project".into()],
                })
            })
            .unwrap();
        let restored = LanStore::load(directory.path()).unwrap();
        assert_eq!(restored.snapshot().remotes[0].token, "secret");
        assert_eq!(
            restored.snapshot().hosting.unwrap().projects,
            vec!["/project"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        restored
            .update(|s| {
                s.remotes.clear();
                s.hosting = None;
            })
            .unwrap();
        assert!(LanStore::load(directory.path())
            .unwrap()
            .snapshot()
            .remotes
            .is_empty());
        assert!(LanStore::load(directory.path())
            .unwrap()
            .snapshot()
            .hosting
            .is_none());
    }
}

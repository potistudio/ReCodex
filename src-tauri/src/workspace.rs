use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{Manager, State};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub history_paths: Vec<String>,
}
#[derive(Default)]
pub struct Workspace(pub Mutex<Vec<Project>>);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    path: String,
    is_directory: bool,
}
fn storage(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    Ok(directory.join("projects.json"))
}
fn persist(app: &tauri::AppHandle, projects: &[Project]) -> Result<(), String> {
    fs::write(
        storage(app)?,
        serde_json::to_vec_pretty(projects).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn projects_load(
    app: tauri::AppHandle,
    state: State<Workspace>,
) -> Result<Vec<Project>, String> {
    let path = storage(&app)?;
    let projects = if path.exists() {
        serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    *state.0.lock().unwrap() = projects;
    Ok(state.0.lock().unwrap().clone())
}
#[tauri::command]
pub fn project_save(
    app: tauri::AppHandle,
    state: State<Workspace>,
    path: String,
    name: Option<String>,
) -> Result<Vec<Project>, String> {
    let root = fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("Choose a directory".into());
    }
    // Keep conventional Windows paths in app-server cwd filters.
    let path = root
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_owned();
    let name = name.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        root.file_name()
            .unwrap_or(root.as_os_str())
            .to_string_lossy()
            .into_owned()
    });
    let mut projects = state.0.lock().unwrap();
    let mut updated = projects.clone();
    if let Some(project) = updated.iter_mut().find(|p| p.path == path) {
        project.name = name;
    } else {
        updated.push(Project {
            path,
            name,
            history_paths: Vec::new(),
        });
    }
    persist(&app, &updated)?;
    *projects = updated;
    Ok(projects.clone())
}
#[tauri::command]
pub fn project_remove(
    app: tauri::AppHandle,
    state: State<Workspace>,
    path: String,
) -> Result<Vec<Project>, String> {
    let mut projects = state.0.lock().unwrap();
    let updated: Vec<_> = projects
        .iter()
        .filter(|p| p.path != path)
        .cloned()
        .collect();
    persist(&app, &updated)?;
    *projects = updated;
    Ok(projects.clone())
}
fn relocate_project(
    projects: &mut Vec<Project>,
    source_path: &str,
    destination_path: String,
) -> Result<Project, String> {
    let mut source_index = projects
        .iter()
        .position(|project| project.path == source_path)
        .ok_or("Open this project first")?;
    if source_path == destination_path {
        return Ok(projects[source_index].clone());
    }
    if let Some(destination_index) = projects
        .iter()
        .position(|project| project.path == destination_path)
    {
        projects.remove(destination_index);
        if destination_index < source_index {
            source_index -= 1;
        }
    }
    let project = &mut projects[source_index];
    if !project.history_paths.contains(&project.path) {
        project.history_paths.push(project.path.clone());
    }
    project.path = destination_path;
    let current_path = project.path.clone();
    project.history_paths.retain(|path| path != &current_path);
    Ok(projects[source_index].clone())
}
#[tauri::command]
pub fn project_relocate(
    app: tauri::AppHandle,
    state: State<Workspace>,
    source_path: String,
    destination_path: String,
) -> Result<Project, String> {
    let root = fs::canonicalize(&destination_path).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("Choose a directory".into());
    }
    let destination_path = root
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_owned();
    let mut projects = state.0.lock().unwrap();
    let mut updated = projects.clone();
    let project = relocate_project(&mut updated, &source_path, destination_path)?;
    persist(&app, &updated)?;
    *projects = updated;
    Ok(project)
}
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if Path::new(relative).is_absolute() {
        return Err("A relative path is required".into());
    }
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let target = fs::canonicalize(root.join(relative)).map_err(|e| e.to_string())?;
    if !target.starts_with(&root) {
        return Err("The path is outside this project".into());
    }
    Ok(target)
}
fn registered(state: &Workspace, root: &str, path: &str) -> Result<PathBuf, String> {
    if !state.0.lock().unwrap().iter().any(|p| p.path == root) {
        return Err("Open this project first".into());
    }
    resolve(Path::new(root), path)
}
#[tauri::command]
pub fn files_list(
    state: State<Workspace>,
    root: String,
    path: String,
) -> Result<Vec<Entry>, String> {
    let directory = registered(&state, &root, &path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if [".git", "node_modules", "target", ".svelte-kit"].contains(&name.as_str()) {
            continue;
        }
        let relative = Path::new(&path)
            .join(&name)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(resolved) = resolve(Path::new(&root), &relative) else {
            continue;
        };
        entries.push(Entry {
            name,
            path: relative,
            is_directory: resolved.is_dir(),
        });
    }
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}
fn read_text(path: &Path) -> Result<String, String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 {
        return Err("Files larger than 2 MB cannot be edited".into());
    }
    let text = fs::read_to_string(path).map_err(|_| "This file is not UTF-8 text".to_owned())?;
    if text.contains('\0') {
        return Err("Binary files cannot be edited".into());
    }
    Ok(text)
}
#[tauri::command]
pub fn file_read(state: State<Workspace>, root: String, path: String) -> Result<String, String> {
    read_text(&registered(&state, &root, &path)?)
}
fn save_text(path: &Path, content: &str, expected: &str) -> Result<(), String> {
    if content.len() > 2 * 1024 * 1024 {
        return Err("Files larger than 2 MB cannot be saved".into());
    }
    if read_text(path)? != expected {
        return Err(
            "This file changed on disk. Reload it before saving to avoid overwriting changes."
                .into(),
        );
    }
    fs::write(path, content).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn file_save(
    state: State<Workspace>,
    root: String,
    path: String,
    content: String,
    expected: String,
) -> Result<(), String> {
    save_text(&registered(&state, &root, &path)?, &content, &expected)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_boundaries_and_conflicting_saves() {
        let directory = std::env::temp_dir().join(format!("recodex-test-{}", std::process::id()));
        fs::create_dir_all(directory.join("project")).unwrap();
        let root = directory.join("project");
        let file = root.join("test.txt");
        fs::write(&file, "original").unwrap();
        fs::write(directory.join("outside.txt"), "private").unwrap();
        assert!(resolve(&root, "../outside.txt").is_err());
        assert!(resolve(&root, "test.txt").is_ok());
        assert!(save_text(&file, "new", "stale").is_err());
        assert_eq!(read_text(&file).unwrap(), "original");
        save_text(&file, "new", "original").unwrap();
        assert_eq!(read_text(&file).unwrap(), "new");
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn relocating_a_project_preserves_its_name_and_deduplicates_the_destination() {
        let mut projects = vec![
            Project {
                name: "Current project".into(),
                path: "C:\\source".into(),
                history_paths: Vec::new(),
            },
            Project {
                name: "Existing project".into(),
                path: "C:\\destination".into(),
                history_paths: Vec::new(),
            },
        ];
        let project =
            relocate_project(&mut projects, "C:\\source", "C:\\destination".into()).unwrap();
        assert_eq!(project.name, "Current project");
        assert_eq!(project.path, "C:\\destination");
        assert_eq!(project.history_paths, ["C:\\source"]);
        assert_eq!(projects.len(), 1);
    }
}

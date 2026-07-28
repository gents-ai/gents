//! Co-resident domain plugin for the fixture host.
//!
//! Owns its own storage root (separate from the Gents bridge home), its own
//! command namespace (`plugin:fixture-domain|*`), and emits
//! `fixture-domain://updated` — proving v1 extension = side-by-side plugins,
//! not shared-node schema registration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

const EVENT_UPDATED: &str = "fixture-domain://updated";

#[derive(Debug, Clone)]
pub struct DomainConfig {
    /// Storage root for the domain store. Host must supply a path under its
    /// own app data — never the Gents bridge home.
    pub home: PathBuf,
}

#[derive(Debug, Default)]
struct DomainState {
    home: PathBuf,
    docs: Mutex<BTreeMap<String, DomainDoc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainDoc {
    pub id: String,
    pub body: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainUpdateEvent {
    pub reason: String,
    pub doc_id: Option<String>,
}

pub fn init<R: Runtime>(config: DomainConfig) -> TauriPlugin<R> {
    let home = config.home;
    Builder::<R>::new("fixture-domain")
        .setup(move |app, _api| {
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            let docs = load_docs(&home).unwrap_or_default();
            app.manage(DomainState {
                home,
                docs: Mutex::new(docs),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            domain_doc_put,
            domain_doc_get,
            domain_doc_list,
            domain_home_path
        ])
        .build()
}

fn store_path(home: &Path) -> PathBuf {
    home.join("docs.json")
}

fn load_docs(home: &Path) -> Result<BTreeMap<String, DomainDoc>, String> {
    let path = store_path(home);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn persist(home: &Path, docs: &BTreeMap<String, DomainDoc>) -> Result<(), String> {
    let path = store_path(home);
    let bytes = serde_json::to_vec_pretty(docs).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

#[tauri::command]
fn domain_home_path(state: State<'_, DomainState>) -> Result<String, String> {
    Ok(state.home.display().to_string())
}

#[tauri::command]
fn domain_doc_list(state: State<'_, DomainState>) -> Result<Vec<DomainDoc>, String> {
    let docs = state.docs.lock().map_err(|e| e.to_string())?;
    Ok(docs.values().cloned().collect())
}

#[tauri::command]
fn domain_doc_get(id: String, state: State<'_, DomainState>) -> Result<Option<DomainDoc>, String> {
    let docs = state.docs.lock().map_err(|e| e.to_string())?;
    Ok(docs.get(&id).cloned())
}

#[tauri::command]
fn domain_doc_put<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    body: String,
    state: State<'_, DomainState>,
) -> Result<DomainDoc, String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("id is required".into());
    }
    let doc = DomainDoc {
        id: id.clone(),
        body,
        updated_at: chrono_now(),
    };
    {
        let mut docs = state.docs.lock().map_err(|e| e.to_string())?;
        docs.insert(id.clone(), doc.clone());
        persist(&state.home, &docs)?;
    }
    let _ = app.emit(
        EVENT_UPDATED,
        DomainUpdateEvent {
            reason: "store".into(),
            doc_id: Some(id),
        },
    );
    Ok(doc)
}

fn chrono_now() -> String {
    // Avoid chrono dep: RFC3339-ish via system time.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_get_round_trip_under_fixed_home() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("domain");
        std::fs::create_dir_all(&home).unwrap();
        let mut docs = BTreeMap::new();
        let doc = DomainDoc {
            id: "inv-1".into(),
            body: r#"{"item":"milk"}"#.into(),
            updated_at: "1".into(),
        };
        docs.insert(doc.id.clone(), doc.clone());
        persist(&home, &docs).unwrap();
        let loaded = load_docs(&home).unwrap();
        assert_eq!(loaded.get("inv-1").unwrap().body, r#"{"item":"milk"}"#);
        assert!(home.join("docs.json").exists());
    }
}

use rapidhash::RapidHashMap;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Placement {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

const PLACES_FILE: &str = "plugin-windows.json";
const VISIBLE: f64 = 80.0;
static PLACES: tokio::sync::Mutex<Option<RapidHashMap<String, Placement>>> =
    tokio::sync::Mutex::const_new(None);

impl Placement {
    pub(super) fn of<R>(window: &tauri::WebviewWindow<R>) -> Option<Self>
    where
        R: Runtime,
    {
        let scale = window.scale_factor().ok()?;
        let at = window.outer_position().ok()?.to_logical::<f64>(scale);
        let size = window.inner_size().ok()?.to_logical::<f64>(scale);
        Some(Self {
            x: at.x,
            y: at.y,
            width: size.width,
            height: size.height,
        })
    }
    pub(super) fn on_screen(&self, screens: &[(f64, f64, f64, f64)]) -> bool {
        screens.iter().any(|&(x, y, w, h)| {
            let across = (self.x + self.width).min(x + w) - self.x.max(x);
            let down = (self.y + self.height).min(y + h) - self.y.max(y);
            across >= VISIBLE && down >= VISIBLE
        })
    }
}

pub(super) fn screens<R>(app: &AppHandle<R>) -> Vec<(f64, f64, f64, f64)>
where
    R: Runtime,
{
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let scale = monitor.scale_factor();
            let at = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            (at.x, at.y, size.width, size.height)
        })
        .collect()
}

fn places_path<R>(app: &AppHandle<R>) -> Option<std::path::PathBuf>
where
    R: Runtime,
{
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(PLACES_FILE))
}

async fn loaded<R>(
    app: &AppHandle<R>,
    places: &mut Option<RapidHashMap<String, Placement>>,
) -> std::path::PathBuf
where
    R: Runtime,
{
    let path = places_path(app).unwrap_or_default();
    if places.is_none() {
        let read = tokio::fs::read(&path).await.ok();
        *places = Some(
            read.and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
        );
    }
    path
}

pub(super) async fn placement_of<R>(app: &AppHandle<R>, plugin_id: &str) -> Option<Placement>
where
    R: Runtime,
{
    let mut places = PLACES.lock().await;
    loaded(app, &mut places).await;
    places.as_ref()?.get(plugin_id).copied()
}

pub(super) fn remember<R>(app: &AppHandle<R>, plugin_id: String, place: Placement)
where
    R: Runtime,
{
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut places = PLACES.lock().await;
        let path = loaded(&app, &mut places).await;
        let Some(map) = places.as_mut() else { return };
        map.insert(plugin_id, place);
        let Ok(bytes) = serde_json::to_vec_pretty(map) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        if let Err(error) = tokio::fs::write(&path, bytes).await {
            tracing::warn!(%error, "could not remember a plugin window's place");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_place_only_while_enough_of_it_is_on_a_screen() {
        let place = |x, y| Placement {
            x,
            y,
            width: 800.0,
            height: 600.0,
        };
        let laptop = (0.0, 0.0, 1512.0, 982.0);
        let beside = (1512.0, 0.0, 1920.0, 1080.0);
        assert!(place(100.0, 100.0).on_screen(&[laptop]));
        assert!(!place(2000.0, 100.0).on_screen(&[laptop]));
        assert!(place(2000.0, 100.0).on_screen(&[laptop, beside]));
        assert!(!place(1480.0, 100.0).on_screen(&[laptop]));
        assert!(!place(100.0, 100.0).on_screen(&[]));
    }

    #[test]
    fn reads_and_writes_places_as_plain_json() {
        let mut places = RapidHashMap::default();
        places.insert(
            "acme/board/page".to_owned(),
            Placement {
                x: 10.0,
                y: 20.0,
                width: 1100.0,
                height: 800.0,
            },
        );
        let bytes = serde_json::to_vec(&places).unwrap();
        let back: RapidHashMap<String, Placement> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, places);
    }
}

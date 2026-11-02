use crate::types::*;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use tauri::State;
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use gents_server::packs::{self, EditedDocuments};

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "packs install into a local Node; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

/// Runs a pack operation on a blocking thread that drives it to completion:
/// opening the local node holds state that must stay on one thread, which a
/// command future may not. Commands use the runtime `install_runtime` registers.
async fn run<T, F, Fut>(operation: F) -> Result<T, BridgeError>
where
    T: DeserializeOwned,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    run_on(tauri::async_runtime::handle().inner(), operation).await
}

async fn run_on<T, F, Fut>(runtime: &tokio::runtime::Handle, operation: F) -> Result<T, BridgeError>
where
    T: DeserializeOwned,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    gents::store::local::storage_backend::drive_unsendable(runtime, operation)
        .await
        .map_err(|error| BridgeError::untyped(format!("the pack operation stopped: {error}")))?
        .map_err(failed)
        .and_then(|report| {
            serde_json::from_value(report)
                .map_err(|error| BridgeError::untyped(format!("invalid pack report: {error}")))
        })
}

#[tauri::command]
pub async fn desktop_pack_installed(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<InstalledPacksView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::installed(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_search(
    query: String,
    page: Option<u32>,
    registry: Option<String>,
) -> Result<PackSearchView, BridgeError> {
    {
        run(move || packs::search(registry, query, page.unwrap_or(1).max(1))).await
    }
}

#[tauri::command]
pub async fn desktop_pack_info(
    package: String,
    registry: Option<String>,
) -> Result<PackInfoView, BridgeError> {
    {
        run(move || packs::info(registry, package)).await
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PackInstallRequest {
    pub package: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub registry: Option<String>,
    #[serde(default)]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub inference_slots: Vec<String>,
    #[serde(default)]
    #[ts(as = "Option<EditedDocuments>", optional)]
    pub edited: EditedDocuments,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub grant_authority: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub preview: bool,
}

#[tauri::command]
pub async fn desktop_pack_install(
    request: PackInstallRequest,
    state: State<'_, DesktopAppState>,
) -> Result<PackInstallView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || {
            packs::install(
                home,
                request.package,
                request.registry,
                request.inference_slots,
                request.edited,
                request.grant_authority,
                request.preview,
            )
        })
        .await
    }
}

#[tauri::command]
pub async fn desktop_pack_update(
    package: Option<String>,
    registry: Option<String>,
    edited: Option<EditedDocuments>,
    state: State<'_, DesktopAppState>,
) -> Result<PackUpdateView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::update(home, package, registry, edited.unwrap_or_default())).await
    }
}

#[tauri::command]
pub async fn desktop_pack_remove(
    package: String,
    edited: Option<EditedDocuments>,
    state: State<'_, DesktopAppState>,
) -> Result<PackRemoveView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::remove(home, package, edited.unwrap_or_default())).await
    }
}

#[tauri::command]
pub async fn desktop_pack_login(
    token: String,
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<PackLoginView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::login(home, registry, token)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_logout(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<PackLogoutView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::logout(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_whoami(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<PackAccountView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::whoami(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_plugin_slots(
    state: State<'_, DesktopAppState>,
) -> Result<PackPluginSlotsView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::plugin_slots(home)).await
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginBindRequest {
    pub plugin: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub profile: Option<String>,
}

#[tauri::command]
pub async fn desktop_pack_plugin_bind(
    request: PluginBindRequest,
    state: State<'_, DesktopAppState>,
) -> Result<PackPluginBindingView, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::bind_plugin_slot(home, request.plugin, request.profile)).await
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PackWireRequest {
    /// An installed plugin, as `namespace/name`.
    pub plugin: String,
    pub agent: String,
    /// Installed packs whose AgentTargets the agent should reach.
    #[serde(default)]
    pub targets_from: Vec<String>,
    #[serde(default)]
    pub remove: bool,
    /// Report what would be written, writing nothing.
    #[serde(default)]
    pub preview: bool,
}

#[tauri::command]
pub async fn desktop_pack_wire(
    request: PackWireRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || {
            packs::wire(
                home,
                request.plugin,
                request.agent,
                request.targets_from,
                request.remove,
                request.preview,
            )
        })
        .await
    }
}

#[tauri::command]
pub async fn desktop_pack_wire_sources(
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::wire_sources(home)).await
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPromoteRequest {
    /// A dynamic run's address (SESSION_ID:TOOL_CALL_ID) or tool call id.
    pub selector: String,
    pub name: Option<String>,
    pub agent: Option<String>,
}

#[tauri::command]
pub async fn desktop_workflows_dynamic(
    state: State<'_, DesktopAppState>,
) -> Result<DynamicWorkflowsView, BridgeError> {
    let home = home(&state)?;
    run(move || packs::dynamic_workflows(home)).await
}

#[tauri::command]
pub async fn desktop_workflow_dynamic(
    selector: String,
    state: State<'_, DesktopAppState>,
) -> Result<DynamicWorkflowView, BridgeError> {
    let home = home(&state)?;
    run(move || packs::dynamic_workflow(home, selector)).await
}

#[tauri::command]
pub async fn desktop_workflow_promote(
    request: WorkflowPromoteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<WorkflowPromoteView, BridgeError> {
    let home = home(&state)?;
    run(move || packs::promote(home, request.selector, request.name, request.agent)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::support::runtime_handle;

    #[tokio::test]
    async fn typed_pack_reports_use_the_existing_operations() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_path_buf();
        let slots: PackPluginSlotsView =
            run_on(&runtime_handle(), move || packs::plugin_slots(path))
                .await
                .unwrap();
        assert!(slots.plugins.is_empty());
        assert!(slots.profiles.is_empty());
        let path = home.path().to_path_buf();
        let report: PackLogoutView = run_on(&runtime_handle(), move || {
            packs::logout(path, Some("https://registry.example".into()))
        })
        .await
        .unwrap();
        assert_eq!(report.registry, "https://registry.example");
        assert!(!report.signed_out);
    }

    #[tokio::test]
    async fn search_report_preserves_registry_pagination() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let registry = MockServer::start().await;
        let report =
            serde_json::json!({"packs":[], "has_more":false, "page":2, "per_page":20, "count":0});
        Mock::given(wiremock::matchers::path("/api/v1/packs"))
            .and(wiremock::matchers::query_param("q", "demo"))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(report.clone()))
            .expect(1)
            .mount(&registry)
            .await;
        let url = registry.uri();
        let view: PackSearchView = run_on(&runtime_handle(), move || {
            packs::search(Some(url), "demo".into(), 2)
        })
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(view).unwrap(), report);
    }

    #[test]
    fn registry_reports_preserve_absence_null_and_additional_metadata() {
        for description in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("demo")),
        ] {
            let mut report = serde_json::json!({"namespace":"example", "name":"demo", "published":{"version":"1.0.0"}});
            if let Some(description) = description {
                report["description"] = description;
            }
            let view: crate::types::FoundPackView = serde_json::from_value(report.clone()).unwrap();
            assert_eq!(serde_json::to_value(view).unwrap(), report);
        }
    }

    #[test]
    fn install_reports_retain_nested_owner_data() {
        let report = serde_json::json!({
            "pack":"demo", "digest":"sha256:digest", "source":"directory",
            "apply": {"created": ["Task/example"], "replaced": []},
            "inference":{"bindings":{"primary":"local"}},
        });
        let view: PackInstallView = serde_json::from_value(report.clone()).unwrap();
        assert_eq!(serde_json::to_value(view).unwrap(), report);
    }
}

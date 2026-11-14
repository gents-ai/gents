use crate::types::*;
use futures::TryFutureExt;
use gents_pack::pack::PackChangeError;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use tauri::State;
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use gents_protocol::operations::pack::EditedDocuments;
use gents_server::packs;

pub(super) fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
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
pub(super) async fn run<T, F, Fut>(operation: F) -> Result<T, BridgeError>
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
    gents_store::local::storage_backend::drive_unsendable(runtime, operation)
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
        if let Some(core) = current_core(&state) {
            let node_did = packs::ux_installing_node(&home).map_err(failed)?;
            let access = gents_store::config_client::ConfigAccess::Graphql(
                core.local_home_operator_endpoint(&node_did, &home)
                    .map_err(|error| failed(error.into()))?,
            );
            run(move || packs::installed_with_access(home, registry, access, node_did)).await
        } else {
            run(move || packs::installed(home, registry)).await
        }
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

/// The Engineer's core packs as the local node's home has them. A read: it
/// works while the node runs.
#[tauri::command]
pub async fn desktop_default_packs(
    state: State<'_, DesktopAppState>,
) -> Result<DefaultPacksView, BridgeError> {
    let home = home(&state)?;
    run(move || packs::core_packs(home)).await
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DefaultPacksApproveRequest {
    pub packs: Vec<DefaultPackApproval>,
}

/// One core pack to turn on, installing it from the release when the home
/// lacks it.
#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DefaultPackApproval {
    pub pack: String,
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
    /// The content digest the install must have: the one its preview
    /// reported, so what installs is what was reviewed.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub expect_digest: Option<String>,
    /// The folder access the review showed, as `PACK=AGENT` for each of its
    /// rows: an allowed install lets only these agents into the folder. An
    /// install from a review always sends it, empty when none was shown.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub root_agents: Option<Vec<String>>,
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
                request.expect_digest,
                request.root_agents,
            )
            .map_err(PackChangeError::into_anyhow)
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
        run(move || {
            packs::update(home, package, registry, edited.unwrap_or_default())
                .map_err(PackChangeError::into_anyhow)
        })
        .await
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
        run(move || {
            packs::remove(home, package, edited.unwrap_or_default())
                .map_err(PackChangeError::into_anyhow)
        })
        .await
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

    #[test]
    fn preview_reports_carry_the_review_and_unknown_collections() {
        let report = serde_json::json!({
            "pack":"demo", "digest":"sha256:digest", "source":"local", "would_write": false,
            "coordinate":"acme/demo", "version":"1.0.0", "description":"Demo", "kind":"documents",
            "documents":[{"collection":"Agent", "id":"a", "name":null}],
            "collections": null,
            "authority":[{"plugin":"fetch", "asks":"HTTP to any host"}],
            "dependencies": [],
        });
        let view: PackInstallView = serde_json::from_value(report.clone()).unwrap();
        assert_eq!(view.collections, Some(None));
        assert_eq!(view.authority.as_ref().map(Vec::len), Some(1));
        assert_eq!(serde_json::to_value(view).unwrap(), report);
    }

    /// A preview's folder access reaches the review typed, so the consent
    /// can name it; an install's per-agent steps pass through as reported.
    #[test]
    fn preview_reports_carry_the_folder_its_agents_would_read() {
        use crate::types::PackHostToolRootsView;
        let report = serde_json::json!({
            "pack":"hands", "digest":"sha256:digest", "source":"local", "would_write": false,
            "host_tool_roots": {
                "tools": ["hands-tools"], "agents": ["Hands worker"],
                "rows": [{"pack": "acme/hands", "agent": "hands-worker",
                    "name": "Hands worker", "tools": "hands-tools"}],
                "root": "/home/me/work", "reader_on": "/home/me",
                "granted_with": "--grant-authority",
            },
        });
        let view: PackInstallView = serde_json::from_value(report.clone()).unwrap();
        let Some(PackHostToolRootsView::Review(review)) = &view.host_tool_roots else {
            panic!("the preview's folder access was not typed: {view:?}")
        };
        assert_eq!(review.agents, ["Hands worker"]);
        assert_eq!(review.rows[0].pack, "acme/hands");
        assert_eq!(review.rows[0].agent, "hands-worker");
        assert_eq!(review.reader_on.as_deref(), Some("/home/me"));
        assert_eq!(serde_json::to_value(view).unwrap(), report);

        let installed = serde_json::json!({
            "pack":"hands", "digest":"sha256:digest", "source":"local",
            "host_tool_roots": [
                {"agent": "did:key:worker", "root": "/home/me/work", "reader_on": "/home/me"},
            ],
        });
        let view: PackInstallView = serde_json::from_value(installed.clone()).unwrap();
        assert!(matches!(
            view.host_tool_roots,
            Some(PackHostToolRootsView::Steps(_))
        ));
        assert_eq!(serde_json::to_value(view).unwrap(), installed);
    }

    #[test]
    fn preview_reports_carry_the_slots_still_to_bind() {
        let report = serde_json::json!({
            "pack":"code_review", "digest":"sha256:digest", "source":"registry",
            "would_write": false,
            "inference_needs": {
                "slots": [{
                    "name":"coordinator", "description":"Plans the review", "optional": false,
                    "packs":["gents/code_review"], "profile": null, "automatic": false,
                }],
                "profiles": [],
                "unbound": ["coordinator"],
            },
        });
        let view: PackInstallView = serde_json::from_value(report.clone()).unwrap();
        let needs = view.inference_needs.as_ref().unwrap();
        assert_eq!(needs.unbound, ["coordinator"]);
        assert_eq!(needs.slots[0].profile, None);
        assert_eq!(serde_json::to_value(view).unwrap(), report);
    }

    #[test]
    fn install_requests_carry_the_reviewed_digest() {
        let request: PackInstallRequest = serde_json::from_value(serde_json::json!({
            "package": "/tmp/demo.pack", "expectDigest": "sha256:abc", "edited": "keep",
        }))
        .unwrap();
        assert_eq!(request.expect_digest.as_deref(), Some("sha256:abc"));
        assert_eq!(request.edited, EditedDocuments::Keep);
        let request: PackInstallRequest =
            serde_json::from_value(serde_json::json!({"package": "acme/demo"})).unwrap();
        assert_eq!(request.expect_digest, None);
    }
}

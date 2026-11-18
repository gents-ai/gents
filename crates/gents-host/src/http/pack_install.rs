use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{rejection::BytesRejection, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{display::Base64Display, engine::general_purpose::STANDARD, Engine};
use gents_pack::pack::archive::PackArchive;
use gents_pack::pack::PackKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::router::RuntimeHttpState;
use gents_pack::pack::install_flow::DocumentsInstallRequest;
use gents_pack::pack::resolved_pack::ResolvedPack;

pub mod failure;
pub mod session;

/// The root and its dependencies share one pack's compressed bound, so a
/// remote install never buffers more archive bytes than one maximal pack.
pub const MAX_ARCHIVE_BYTES: usize = gents_pack::pack::archive::MAX_PACK_BYTES;
/// Room for JSON framing and option strings beside the encoded archives.
const MAX_FIELD_BYTES: usize = 1024 * 1024;
pub const MAX_BODY: usize = MAX_ARCHIVE_BYTES.div_ceil(3) * 4 + MAX_FIELD_BYTES;

/// Packed archive bytes, carried as base64 without an intermediate string.
struct Archive(Vec<u8>);

impl Serialize for Archive {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&Base64Display::new(&self.0, &STANDARD))
    }
}

impl<'de> Deserialize<'de> for Archive {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Archive;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a base64 pack archive")
            }
            fn visit_str<E>(self, value: &str) -> Result<Archive, E>
            where
                E: serde::de::Error,
            {
                STANDARD.decode(value).map(Archive).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    move_pin_tools: Vec<String>,
    node_did: String,
    root: Archive,
    dependencies: Vec<Archive>,
    root_source: gents_pack::pack::PackSource,
    dependency_sources: Vec<gents_pack::pack::PackSource>,
    inference_slots: Vec<String>,
    dependency_variables: Vec<String>,
    /// `SLOT=AGENT_DID`: the agent slots this install binds to existing
    /// agents. The serving runtime's environment never binds one.
    #[serde(default)]
    bind_agents: Vec<String>,
    overwrite: bool,
    keep: bool,
    preview: bool,
    force_rebind: bool,
    grant_authority: bool,
    explicit: bool,
    wire: Vec<String>,
    /// The client's build identity. A plan is prepared twice, so the runtime
    /// can say which builds prepared the two halves when they disagree.
    /// Required: a peer without it is an older build, and that is refused by
    /// name rather than tolerated silently (no back-compat before 1.0).
    client_build: gents_protocol::build_info::BuildInfo,
}

/// Repacks a verified archive from its declared files and confirms the
/// content digest the operator resolved.
fn pack_archive(archive: &PackArchive) -> Result<Vec<u8>> {
    let temp = tempfile::tempdir()?;
    archive.write_to(temp.path())?;
    let mut bytes = Vec::new();
    let header = gents_pack::pack::archive::write_pack(temp.path(), &mut bytes)?;
    anyhow::ensure!(
        header.digest == archive.digest(),
        "repacked archive digest changed"
    );
    Ok(bytes)
}

fn ensure_archive_bound(total: usize) -> Result<()> {
    anyhow::ensure!(
        total <= MAX_ARCHIVE_BYTES,
        "remote pack install carries {total} compressed archive bytes, over the \
         {MAX_ARCHIVE_BYTES} byte bound shared by a pack and its dependencies; nothing was installed"
    );
    Ok(())
}

fn encode_request(
    root: &PackArchive,
    dependencies: &[Arc<PackArchive>],
    fields: impl FnOnce(Archive, Vec<Archive>) -> InstallRequest,
) -> Result<Vec<u8>> {
    let root = Archive(pack_archive(root)?);
    let dependencies = dependencies
        .iter()
        .map(|archive| pack_archive(archive).map(Archive))
        .collect::<Result<Vec<_>>>()?;
    ensure_archive_bound(root.0.len() + dependencies.iter().map(|a| a.0.len()).sum::<usize>())?;
    Ok(serde_json::to_vec(&fields(root, dependencies))?)
}

pub async fn encode_install_request(
    pack: &ResolvedPack,
    dependencies: &[ResolvedPack],
    args: &DocumentsInstallRequest,
    owner: &str,
    explicit: bool,
    // The build preparing this half of the plan. The caller supplies it,
    // because the core pack archives live in the binary, not in this crate.
    client_build: gents_protocol::build_info::BuildInfo,
) -> Result<serde_json::Value> {
    let root = pack.shared_archive();
    let root_source = pack.provenance();
    let dependency_sources = dependencies
        .iter()
        .map(|source| source.provenance())
        .collect();
    let dependencies = dependencies
        .iter()
        .map(ResolvedPack::shared_archive)
        .collect::<Vec<_>>();
    let node_did = owner.to_owned();
    let inference_slots = args.inference_slots.clone();
    let dependency_variables = args.dependency_variables.clone();
    let bind_agents = args.bind_agents.clone();
    let wire = args.wire.clone();
    let move_pin_tools = args.move_pin_tools.clone();
    let (overwrite, keep, preview, force_rebind, grant_authority) = (
        args.edited == gents_protocol::operations::pack::EditedDocuments::Overwrite,
        args.edited == gents_protocol::operations::pack::EditedDocuments::Keep,
        args.preview,
        args.force_rebind_concrete_did,
        args.grant_authority,
    );
    tokio::task::spawn_blocking(move || {
        let body = encode_request(&root, &dependencies, |root, dependencies| InstallRequest {
            move_pin_tools,
            node_did,
            root,
            dependencies,
            root_source,
            dependency_sources,
            inference_slots,
            dependency_variables,
            bind_agents,
            overwrite,
            keep,
            preview,
            force_rebind,
            grant_authority,
            explicit,
            wire,
            // So a disagreement names both halves' builds, not only that they
            // differ.
            client_build,
        })?;
        Ok::<_, anyhow::Error>(serde_json::from_slice(&body)?)
    })
    .await
    .context("packing remote document archives")?
}

/// A refusal before an install or session upgrade can proceed.
fn refused(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({"error": format!("{error:#}"), "outcome": "not_installed"})),
    )
        .into_response()
}

pub async fn install_handler(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(actor) = identity.did().cloned() else {
        return refused(
            StatusCode::UNAUTHORIZED,
            "Client authentication is required.",
        );
    };
    let Some(home) = state.home.clone() else {
        return refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime plugin store unavailable",
        );
    };
    let access = match delegated_install_access(&state, &headers, actor) {
        Ok(access) => access,
        Err(error) => return refused(StatusCode::UNAUTHORIZED, error),
    };
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return refused(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "remote pack install request exceeds the {MAX_BODY} byte bound \
                     ({MAX_ARCHIVE_BYTES} compressed archive bytes)"
                ),
            )
        }
        Err(rejection) => return refused(rejection.status(), rejection.body_text()),
    };
    let node_did = state.node_did.clone();
    let received = match tokio::task::spawn_blocking(move || decode_request(&body, &node_did)).await
    {
        Ok(Ok(received)) => received,
        Ok(Err(error)) => return refused(StatusCode::BAD_REQUEST, error),
        Err(error) => return refused(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if received.root.manifest().metadata.kind != PackKind::Documents {
        return refused(
            StatusCode::BAD_REQUEST,
            "plugin packs require the reviewed install session",
        );
    }
    // Before anything is planned: a plan prepared by two builds is not one plan.
    let runtime_build = match state.pack_install_build().await {
        Ok(build) => build,
        Err(error) => return refused(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if let Some(refusal) = refuse_client_build(&received.request.client_build, &runtime_build) {
        return refused(StatusCode::CONFLICT, refusal);
    }
    // Detached so a dropped client cannot cancel an install mid-commit.
    let result = tokio::spawn(async move {
        install_owned(state, PathBuf::from(home), received, access)
            .await
            .map_err(gents_pack::pack::PackChangeError::into_anyhow)
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": format!("{error:#}"), "outcome": "unconfirmed"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("pack install outcome unconfirmed: {error}"),
                "outcome": "unconfirmed",
            })),
        )
            .into_response(),
    }
}

fn delegated_install_access(
    state: &RuntimeHttpState,
    headers: &HeaderMap,
    actor: identity::Did,
) -> Result<gents_store::config_client::ConfigAccess> {
    Ok(gents_store::config_client::ConfigAccess::Graphql(
        crate::http::private_reader::caller_endpoint(state, headers, actor)?,
    ))
}

struct Received {
    root: ResolvedPack,
    dependencies: Vec<ResolvedPack>,
    request: InstallRequest,
    identifiers: failure::RefusalIdentifiers,
}

fn decode_request(body: &[u8], node_did: &str) -> Result<Received> {
    let request: InstallRequest =
        serde_json::from_slice(body).context("decoding pack install request")?;
    decode_install_request(request, node_did)
}

fn decode_install_request(mut request: InstallRequest, node_did: &str) -> Result<Received> {
    anyhow::ensure!(
        request.node_did == node_did,
        "endpoint serves a different node"
    );
    anyhow::ensure!(
        !(request.keep && request.overwrite),
        "conflicting drift policies"
    );
    ensure_archive_bound(
        request.root.0.len()
            + request
                .dependencies
                .iter()
                .map(|archive| archive.0.len())
                .sum::<usize>(),
    )?;
    anyhow::ensure!(
        request.dependency_sources.len() == request.dependencies.len(),
        "dependency source count does not match archives"
    );
    let open = |archive: Archive, source| -> Result<ResolvedPack> {
        ResolvedPack::received(PackArchive::from_bytes(&archive.0)?, source)
    };
    let root = open(
        std::mem::replace(&mut request.root, Archive(Vec::new())),
        request.root_source.clone(),
    )?;
    let dependencies = std::mem::take(&mut request.dependencies)
        .into_iter()
        .zip(std::mem::take(&mut request.dependency_sources))
        .map(|(archive, source)| open(archive, source))
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        matches!(
            root.manifest().metadata.kind,
            PackKind::Documents | PackKind::Plugins | PackKind::Assets
        ),
        "unsupported remote pack root"
    );
    ensure_exact_closure(&root, &dependencies)?;
    let identifiers = failure::RefusalIdentifiers::from_packs(
        &std::iter::once(&root)
            .chain(dependencies.iter())
            .collect::<Vec<_>>(),
    );
    Ok(Received {
        identifiers,
        root,
        dependencies,
        request,
    })
}

/// The received dependencies are exactly the root manifest's dependency list.
fn ensure_exact_closure(root: &ResolvedPack, dependencies: &[ResolvedPack]) -> Result<()> {
    let mut expected = root
        .manifest()
        .metadata
        .dependencies
        .iter()
        .map(|coordinate| {
            let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(coordinate);
            format!("{namespace}/{name}")
        })
        .collect::<Vec<_>>();
    let mut actual = dependencies
        .iter()
        .map(|source| {
            format!(
                "{}/{}",
                source.manifest().metadata.namespace,
                source.manifest().name
            )
        })
        .collect::<Vec<_>>();
    expected.sort();
    actual.sort();
    anyhow::ensure!(
        actual == expected,
        "archive dependencies {actual:?} do not match the root manifest's {expected:?}"
    );
    Ok(())
}

async fn install_owned(
    state: RuntimeHttpState,
    home: PathBuf,
    received: Received,
    access: gents_store::config_client::ConfigAccess,
) -> std::result::Result<serde_json::Value, gents_pack::pack::PackChangeError> {
    let Received {
        root,
        dependencies,
        request,
        ..
    } = received;
    let explicit = request.explicit;
    let args = DocumentsInstallRequest {
        move_pin_tools: request.move_pin_tools.clone(),
        home: home.clone(),
        remote: false,
        inference_slots: request.inference_slots,
        dependency_variables: request.dependency_variables,
        bind_agents: request.bind_agents,
        preview: request.preview,
        force_rebind_concrete_did: request.force_rebind,
        edited: gents_protocol::operations::pack::EditedDocuments::of_switches(
            request.overwrite,
            request.keep,
        ),
        grant_authority: request.grant_authority,
        wire: request.wire,
    };
    let mut result = gents_pack::pack::install_flow::install_documents_pack_resolved(
        &access,
        &state.node_did,
        &root,
        &args,
        explicit,
        dependencies,
        &gents_tools::surface::PackRoots,
    )
    .await?;
    if !args.wire.is_empty() && !args.preview {
        result["wired"] = gents_pack::pack::plugin_wire::wire_pack_plugins(
            &access,
            &state.node_did,
            &home,
            &root,
            &args.wire,
            gents_service::home_state::load_held_user_identity,
        )
        .await?
        .into();
    }
    Ok(result)
}

#[cfg(test)]
#[path = "pack_install_tests.rs"]
mod tests;

/// The build of the process serving installs, as the binary registered it.
///
/// The binary registers its version facts at startup, before any command runs,
/// and the function that digests its core-pack archives. The digests are
/// computed only when an install handshake first asks for the build, so a
/// command that never serves an install hashes no archive.
pub struct RuntimeBuildSource {
    facts: gents_protocol::build_info::BuildInfo,
    core_packs: Box<dyn Fn() -> Vec<gents_protocol::build_info::CorePackBuild> + Send + Sync>,
    resolved: std::sync::OnceLock<gents_protocol::build_info::BuildInfo>,
}

impl RuntimeBuildSource {
    /// The version facts now, the core-pack digests on first use.
    pub fn new<F>(facts: gents_protocol::build_info::BuildInfo, core_packs: F) -> Self
    where
        F: Fn() -> Vec<gents_protocol::build_info::CorePackBuild> + Send + Sync + 'static,
    {
        Self {
            facts,
            core_packs: Box::new(core_packs),
            resolved: std::sync::OnceLock::new(),
        }
    }

    /// The whole build, digesting the core packs the first time it is asked.
    pub fn build(&self) -> &gents_protocol::build_info::BuildInfo {
        self.resolved.get_or_init(|| {
            let mut build = self.facts.clone();
            build.core_packs = (self.core_packs)();
            build
        })
    }

    pub async fn build_async(self: Arc<Self>) -> Result<gents_protocol::build_info::BuildInfo> {
        tokio::task::spawn_blocking(move || self.build().clone())
            .await
            .context("runtime build computation failed")
    }

    /// Whether the core-pack digests have been computed yet.
    pub fn digests_computed(&self) -> bool {
        self.resolved.get().is_some()
    }
}

static RUNTIME_BUILD: std::sync::OnceLock<Arc<RuntimeBuildSource>> = std::sync::OnceLock::new();

/// Record this process's build: its version facts, and how to digest its core
/// packs when an install first needs them. Called once by the binary before
/// any command runs; a later call keeps the first. For the binary only: a
/// test gives its router a build instead (`RuntimeHttpState::pack_install_build`).
pub fn record_runtime_build(source: RuntimeBuildSource) {
    let _ = RUNTIME_BUILD.set(Arc::new(source));
}

/// The registered source, if the binary registered one.
pub fn runtime_build_source() -> Option<&'static RuntimeBuildSource> {
    RUNTIME_BUILD.get().map(Arc::as_ref)
}

/// This process's build as the handshake reports it. A process that recorded
/// none states that plainly, so the refusal says "unverifiable" instead of
/// reporting a build whose commit was never captured.
pub fn runtime_build() -> gents_protocol::build_info::BuildInfo {
    RUNTIME_BUILD
        .get()
        .map(|source| source.build().clone())
        .unwrap_or(gents_protocol::build_info::BuildInfo::UNVERIFIABLE)
}

pub async fn runtime_build_async() -> Result<gents_protocol::build_info::BuildInfo> {
    match RUNTIME_BUILD.get() {
        Some(source) => Arc::clone(source).build_async().await,
        None => Ok(gents_protocol::build_info::BuildInfo::UNVERIFIABLE),
    }
}

/// The runtime's half of the build check: the client's build against the
/// build this router serves installs as (`RuntimeHttpState::pack_install_build`),
/// refused before anything is prepared or written. The refusal's text holds
/// build facts only.
pub(crate) fn refuse_client_build(
    client_build: &gents_protocol::build_info::BuildInfo,
    runtime_build: &gents_protocol::build_info::BuildInfo,
) -> Option<crate::http::pack_install_client::PackInstallHandshakeError> {
    crate::http::pack_install_client::PackInstallHandshakeError::build_mismatch(
        client_build,
        runtime_build,
    )
}

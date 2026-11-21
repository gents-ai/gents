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
use failure::SessionFailure;
use gents_protocol::operations::pack::hosted::HostedInstallFailure as Class;
use gents_pack::pack::install_flow::DocumentsInstallRequest;
use gents_pack::pack::resolved_pack::ResolvedPack;

mod failure;
pub mod session;

const PATH: &str = "/packs/install";
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
    /// The installer's environment value of each variable the packs read: the
    /// only environment the runtime plans this install from.
    installer_environment: std::collections::BTreeMap<String, String>,
    overwrite: bool,
    keep: bool,
    preview: bool,
    force_rebind: bool,
    grant_authority: bool,
    explicit: bool,
    wire: Vec<String>,
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
) -> Result<serde_json::Value> {
    let supplied = gents_pack::pack::install_flow::supplied_variables(
        &args.dependency_variables,
        &args.bind_agents,
    )?;
    let packs = std::iter::once(pack)
        .chain(dependencies.iter())
        .collect::<Vec<_>>();
    let installer_environment =
        gents_pack::pack::install_flow::installer_environment(&packs, &supplied, &|name| {
            args.environment.value(name)
        })?;
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
    let (overwrite, keep, preview, force_rebind, grant_authority) = (
        args.edited == gents_protocol::operations::pack::EditedDocuments::Overwrite,
        args.edited == gents_protocol::operations::pack::EditedDocuments::Keep,
        args.preview,
        args.force_rebind_concrete_did,
        args.grant_authority,
    );
    tokio::task::spawn_blocking(move || {
        let body = encode_request(&root, &dependencies, |root, dependencies| InstallRequest {
            node_did,
            root,
            dependencies,
            root_source,
            dependency_sources,
            inference_slots,
            dependency_variables,
            bind_agents,
            installer_environment,
            overwrite,
            keep,
            preview,
            force_rebind,
            grant_authority,
            explicit,
            wire,
        })?;
        Ok::<_, anyhow::Error>(serde_json::from_slice(&body)?)
    })
    .await
    .context("packing remote document archives")?
}

fn refused(status: StatusCode, class: Class) -> Response {
    failed(status, SessionFailure::not_installed(class)(anyhow::anyhow!("refused")))
}

fn failed(status: StatusCode, failure: SessionFailure) -> Response {
    failure.log();
    (status, Json(failure.body())).into_response()
}

pub async fn install_handler(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(actor) = identity.did().cloned() else {
        return refused(StatusCode::UNAUTHORIZED, Class::NotAuthorized);
    };
    let Some(home) = state.home.clone() else {
        return refused(StatusCode::SERVICE_UNAVAILABLE, Class::RuntimeUnavailable);
    };
    let access = match delegated_install_access(&state, &headers, actor) {
        Ok(access) => access,
        Err(_) => return refused(StatusCode::UNAUTHORIZED, Class::NotAuthorized),
    };
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return refused(rejection.status(), Class::InvalidRequest),
    };
    let node_did = state.node_did.clone();
    let received = match tokio::task::spawn_blocking(move || decode_request(&body, &node_did)).await
    {
        Ok(Ok(received)) => received,
        Ok(Err(_)) => return refused(StatusCode::BAD_REQUEST, Class::InvalidRequest),
        Err(_) => return refused(StatusCode::INTERNAL_SERVER_ERROR, Class::InvalidRequest),
    };
    if (!received.request.wire.is_empty() && !received.request.preview)
        || received.root.manifest().metadata.kind != PackKind::Documents
    {
        return refused(StatusCode::BAD_REQUEST, Class::InvalidRequest);
    }
    let class = if received.request.preview {
        Class::PreviewFailed
    } else {
        Class::ApplyUnconfirmed
    };
    let identifiers = failure::RefusalIdentifiers::of(
        &std::iter::once(&received.root)
            .chain(received.dependencies.iter())
            .collect::<Vec<_>>(),
    );
    // Detached so a dropped client cannot cancel an install mid-commit.
    let result =
        tokio::spawn(
            async move { install_owned(state, PathBuf::from(home), received, access).await },
        )
        .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => failed(
            StatusCode::UNPROCESSABLE_ENTITY,
            SessionFailure::change(class, error, &identifiers),
        ),
        Err(error) => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            SessionFailure::unconfirmed(class)(error),
        ),
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
    Ok(Received {
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
) -> Result<serde_json::Value, gents_pack::pack::PackChangeError> {
    let Received {
        root,
        dependencies,
        request,
    } = received;
    let explicit = request.explicit;
    let args = DocumentsInstallRequest {
        environment: gents_pack::operations::pack::InstallEnvironment::HostedRuntime(
            request.installer_environment,
        ),
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
    gents_pack::pack::install_flow::install_documents_pack_resolved(
        &access,
        &state.node_did,
        &root,
        &args,
        explicit,
        dependencies,
        &gents_tools::surface::PackRoots,
    )
    .await
}

#[cfg(test)]
#[path = "pack_install_tests.rs"]
mod tests;

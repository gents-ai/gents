use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{rejection::BytesRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use base64::{display::Base64Display, engine::general_purpose::STANDARD, Engine};
use gents::pack::archive::PackArchive;
use gents::pack::PackKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::router::RuntimeHttpState;
use crate::cli::output_format::OutputFormat;
use crate::cli::{PackDriftArgs, PackInstallArgs, ScopeArgs};
use crate::commands::pack::PackSource;

const PATH: &str = "/packs/install";
/// The root and its dependencies share one pack's compressed bound, so a
/// remote install never buffers more archive bytes than one maximal pack.
pub(crate) const MAX_ARCHIVE_BYTES: usize = gents::pack::archive::MAX_PACK_BYTES;
/// Room for JSON framing and option strings beside the encoded archives.
const MAX_FIELD_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_BODY: usize = MAX_ARCHIVE_BYTES.div_ceil(3) * 4 + MAX_FIELD_BYTES;

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
    inference_slots: Vec<String>,
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
    let header = gents::pack::archive::write_pack(temp.path(), &mut bytes)?;
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

pub(crate) async fn install_remote(
    endpoint: &gents::config_client::GraphqlEndpoint,
    owner: &str,
    pack: &PackSource,
    dependencies: &[PackSource],
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value> {
    let mut url = reqwest::Url::parse(endpoint.url())?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "remote pack install requires an HTTP URL without credentials"
    );
    let prefix = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("remote pack install requires a Gents runtime GraphQL endpoint")?;
    url.set_path(&format!("{prefix}{PATH}"));
    let root = pack.shared_archive();
    let dependencies = dependencies
        .iter()
        .map(PackSource::shared_archive)
        .collect::<Vec<_>>();
    let node_did = owner.to_owned();
    let inference_slots = args.inference_slots.clone();
    let wire = args.wire.clone();
    let (overwrite, keep, preview, force_rebind, grant_authority) = (
        args.drift.overwrite,
        args.drift.keep,
        args.preview,
        args.force_rebind_concrete_did,
        args.grant_authority,
    );
    let body = tokio::task::spawn_blocking(move || {
        encode_request(&root, &dependencies, |root, dependencies| InstallRequest {
            node_did,
            root,
            dependencies,
            inference_slots,
            overwrite,
            keep,
            preview,
            force_rebind,
            grant_authority,
            explicit,
            wire,
        })
    })
    .await
    .context("packing remote document archives")??;
    let client = tokio::task::spawn_blocking(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
    })
    .await??;
    let response = endpoint
        .authorize(client.post(url))?
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .context(
            "remote pack install outcome is unconfirmed; inspect the runtime before retrying",
        )?;
    let status = response.status();
    let body = response.bytes().await.context(
        "remote pack install response was unreadable; inspect the runtime before retrying",
    )?;
    anyhow::ensure!(
        status.is_success(),
        "remote pack install returned HTTP {status}: {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body)
        .context("remote pack install response was unreadable; inspect the runtime before retrying")
}

fn refused(status: StatusCode, error: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({"error": format!("{error:#}"), "outcome": "not_installed"})),
    )
        .into_response()
}

pub(crate) async fn install_handler(
    State(state): State<RuntimeHttpState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(home) = state.home.clone() else {
        return refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime plugin store unavailable",
        );
    };
    if state.activation_runtime.get().is_none() {
        return refused(StatusCode::SERVICE_UNAVAILABLE, "runtime is not active");
    }
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
    // Detached so a dropped client cannot cancel an install mid-commit.
    let result =
        tokio::spawn(async move { install_owned(state, PathBuf::from(home), received).await })
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

struct Received {
    root: PackSource,
    dependencies: Vec<PackSource>,
    request: InstallRequest,
}

fn decode_request(body: &[u8], node_did: &str) -> Result<Received> {
    let mut request: InstallRequest =
        serde_json::from_slice(body).context("decoding pack install request")?;
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
    let open = |archive: Archive| -> Result<PackSource> {
        Ok(PackSource::received(PackArchive::from_bytes(&archive.0)?))
    };
    let root = open(std::mem::replace(&mut request.root, Archive(Vec::new())))?;
    let dependencies = std::mem::take(&mut request.dependencies)
        .into_iter()
        .map(open)
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        root.manifest().metadata.kind == PackKind::Documents,
        "remote pack install accepts a documents pack root"
    );
    ensure_exact_closure(&root, &dependencies)?;
    Ok(Received {
        root,
        dependencies,
        request,
    })
}

/// The received dependencies are exactly the root manifest's dependency list.
fn ensure_exact_closure(root: &PackSource, dependencies: &[PackSource]) -> Result<()> {
    let mut expected = root
        .manifest()
        .metadata
        .dependencies
        .iter()
        .map(|coordinate| {
            let (namespace, name) = gents::pack::registry::split_pack_coordinate(coordinate);
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
) -> Result<serde_json::Value> {
    let Received {
        root,
        dependencies,
        request,
    } = received;
    let args = PackInstallArgs {
        package: root.manifest().name.clone(),
        inference_slots: request.inference_slots,
        preview: request.preview,
        scope: ScopeArgs {
            home: Some(home.clone()),
            graphql: None,
            node_did: None,
        },
        output: OutputFormat::Json,
        force_rebind_concrete_did: request.force_rebind,
        registry: None,
        drift: PackDriftArgs {
            overwrite: request.overwrite,
            keep: request.keep,
        },
        grant_authority: request.grant_authority,
        wire: request.wire,
        expect_digest: None,
        explicit: request.explicit,
    };
    let access = gents::config_client::ConfigAccess::Graphql(state.p2p_graphql.clone());
    let mut result = crate::commands::pack::install_documents_pack_resolved(
        &access,
        &state.node_did,
        &root,
        &args,
        args.explicit,
        dependencies,
    )
    .await?;
    if !args.wire.is_empty() && !args.preview {
        result["wired"] = crate::commands::pack::wire_pack_plugins(
            &access,
            &state.node_did,
            &home,
            &root,
            &args.wire,
        )
        .await?
        .into();
    }
    Ok(result)
}

#[cfg(test)]
#[path = "pack_install_tests.rs"]
mod tests;

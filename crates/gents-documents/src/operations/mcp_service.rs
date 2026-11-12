use gents_protocol::operations::document::{
    DocumentDeleteReceipt, DocumentOperationError, DocumentReceipt, OperationActor, WriteMode,
};
use gents_protocol::operations::mcp_service::{
    EndpointHost, EndpointScheme, EndpointStorage, McpRegisterInput, McpServiceOperationError,
    StoredToggle, Toggle,
};
use gents_protocol::operations::StringUpdate;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{json, Map, Value};

use super::write::{write_in_txn, DocumentWrite};
use super::OperationFailure;
use crate::inference_backend::AdvertisedProfilePolicy;
use crate::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::{read_desired_state_record_in_txn, ConfigurationNodeBinding, ToolServiceRegistry};

pub type McpServiceFailure = OperationFailure<McpServiceOperationError>;

/// Lean `DocumentOperations.endpointStorage`.
pub fn endpoint_storage(scheme: EndpointScheme, host: EndpointHost) -> EndpointStorage {
    match (scheme, host) {
        (EndpointScheme::Https, _) => EndpointStorage::VerbatimUrl,
        (EndpointScheme::Http, EndpointHost::Ipv4) => EndpointStorage::LanIp,
        (EndpointScheme::Http, EndpointHost::Name) => EndpointStorage::Hostname,
    }
}

/// Lean `DocumentOperations.defaultsPath`.
pub fn defaults_path(scheme: EndpointScheme, path_is_root: bool) -> bool {
    scheme == EndpointScheme::Http && path_is_root
}

/// Lean `DocumentOperations.enabledAfterRegister`.
pub fn enabled_after_register(stored: StoredToggle, requested: Toggle) -> bool {
    requested.requested().unwrap_or(stored != StoredToggle::Off)
}

/// Lean `DocumentOperations.sendNodeDidAfterRegister`.
pub fn send_node_did_after_register(stored: StoredToggle, requested: Toggle) -> bool {
    requested.requested().unwrap_or(stored == StoredToggle::On)
}

fn stored_toggle(stored: Option<&Value>, field: &str, default: bool) -> StoredToggle {
    match stored {
        None => StoredToggle::Absent,
        Some(document) => {
            if document
                .get(field)
                .and_then(Value::as_bool)
                .unwrap_or(default)
            {
                StoredToggle::On
            } else {
                StoredToggle::Off
            }
        }
    }
}

/// The address fields `endpoint` is stored as.
pub fn endpoint_patch(endpoint: &str) -> Result<SelfConfigPatch, McpServiceOperationError> {
    let refused = |error: anyhow::Error| McpServiceOperationError::Endpoint {
        source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
    };
    let url = ToolServiceRegistry::parse_endpoint_url(endpoint).map_err(refused)?;
    let host = url
        .host_str()
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| refused(anyhow::anyhow!("endpoint_url must contain a host")))?;
    let scheme = if url.scheme() == "https" {
        EndpointScheme::Https
    } else {
        EndpointScheme::Http
    };
    let host_kind = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        EndpointHost::Ipv4
    } else {
        EndpointHost::Name
    };
    let storage = endpoint_storage(scheme, host_kind);
    let verbatim = storage == EndpointStorage::VerbatimUrl;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| refused(anyhow::anyhow!("endpoint_url must contain a port")))?;
    let path = url.path().trim();
    let path = if defaults_path(scheme, matches!(path, "" | "/")) {
        "/mcp"
    } else {
        path
    };
    let address = |wanted: EndpointStorage| (storage == wanted).then(|| json!(host));
    Ok(vec![
        ("hostname".to_owned(), address(EndpointStorage::Hostname)),
        ("lan_ip".to_owned(), address(EndpointStorage::LanIp)),
        ("tailscale_ip".to_owned(), None),
        ("mcp_port".to_owned(), (!verbatim).then(|| json!(port))),
        ("mcp_path".to_owned(), (!verbatim).then(|| json!(path))),
        (
            "endpoint_url".to_owned(),
            verbatim.then(|| json!(url.to_string())),
        ),
    ])
}

fn valid(merged: &Map<String, Value>) -> anyhow::Result<()> {
    serde_json::from_value::<ToolServiceRegistry>(Value::Object(merged.clone()))?.validate()
}

pub fn finish(failure: McpServiceFailure) -> McpServiceOperationError {
    failure.finish(|error| DocumentOperationError::Passthrough(error).into())
}

fn text(
    patch: &mut SelfConfigPatch,
    field: &str,
    update: &StringUpdate,
    created_default: Option<&str>,
) {
    match update {
        StringUpdate::Set(value) => patch.push((field.to_owned(), Some(json!(value)))),
        StringUpdate::Clear => patch.push((field.to_owned(), None)),
        StringUpdate::Omitted => {
            if let Some(default) = created_default {
                patch.push((field.to_owned(), Some(json!(default))));
            }
        }
    }
}

async fn register_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    input: &McpRegisterInput,
    node_binding: Option<&ConfigurationNodeBinding>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, McpServiceFailure> {
    let target = SelfConfigTarget::ToolServiceRegistry;
    let service_id = input.service_id.trim();
    let mut patch = endpoint_patch(&input.endpoint).map_err(OperationFailure::Operation)?;
    let stored = read_desired_state_record_in_txn(txn, target.collection(), owner, service_id)
        .await?
        .map(|(_, value)| value);
    let created = stored.is_none();
    text(
        &mut patch,
        "display_name",
        &input.display_name,
        created.then_some(service_id),
    );
    text(&mut patch, "description", &input.description, None);
    patch.push((
        "enabled".to_owned(),
        Some(json!(enabled_after_register(
            stored_toggle(stored.as_ref(), "enabled", true),
            input.enabled
        ))),
    ));
    patch.push((
        "send_node_did".to_owned(),
        Some(json!(send_node_did_after_register(
            stored_toggle(stored.as_ref(), "send_node_did", false),
            input.send_node_did
        ))),
    ));
    let write = DocumentWrite {
        target,
        owner,
        id: service_id,
        mode: WriteMode::Save,
        patch: &patch,
        node_binding,
    };
    write_in_txn(txn, &write, &valid, policy, commit)
        .await
        .map_err(|failure| failure.map(McpServiceOperationError::Document))
}

/// Registers an MCP service of `owner` at an endpoint, or moves a registered
/// one there.
pub async fn register(
    access: &ConfigAccess,
    owner: &str,
    input: &McpRegisterInput,
    node_binding: Option<&ConfigurationNodeBinding>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, McpServiceFailure> {
    if commit {
        access
            .transact_write("operations.mcp_service.register", |txn| {
                Box::pin(register_in_txn(
                    txn,
                    owner,
                    input,
                    node_binding,
                    policy,
                    commit,
                ))
            })
            .await
    } else {
        access
            .transact_readonly("operations.mcp_service.register_preview", |txn| {
                Box::pin(register_in_txn(
                    txn,
                    owner,
                    input,
                    node_binding,
                    policy,
                    commit,
                ))
            })
            .await
    }
}

pub struct McpServiceWrite<'a> {
    pub owner: &'a str,
    pub service_id: &'a str,
    pub mode: WriteMode,
    pub patch: &'a SelfConfigPatch,
    pub node_binding: Option<&'a ConfigurationNodeBinding>,
}

/// Creates or edits one MCP service of `owner` by its fields.
pub async fn save(
    access: &ConfigAccess,
    write: &McpServiceWrite<'_>,
    policy: &dyn AdvertisedProfilePolicy,
    commit: bool,
) -> Result<DocumentReceipt, McpServiceFailure> {
    let document = DocumentWrite {
        target: SelfConfigTarget::ToolServiceRegistry,
        owner: write.owner,
        id: write.service_id,
        mode: write.mode,
        patch: write.patch,
        node_binding: write.node_binding,
    };
    super::write::write(
        access,
        "operations.mcp_service.save",
        &document,
        &valid,
        policy,
        commit,
    )
    .await
    .map_err(|failure| failure.map(McpServiceOperationError::Document))
}

/// The fields a whole service document sets and clears, for a client that
/// saves one.
pub fn document_patch(document: &ToolServiceRegistry) -> anyhow::Result<SelfConfigPatch> {
    let value = serde_json::to_value(document)?;
    Ok(SelfConfigTarget::ToolServiceRegistry
        .writable_fields()
        .into_iter()
        .map(|field| (field.to_owned(), value.get(field).cloned()))
        .collect())
}

/// Deletes one MCP service of `owner` through the one document delete.
pub async fn delete(
    access: &ConfigAccess,
    actor: OperationActor,
    owner: &str,
    service_id: &str,
    digest: Option<&str>,
    policy: &dyn AdvertisedProfilePolicy,
) -> Result<DocumentDeleteReceipt, McpServiceFailure> {
    let targets = [(
        SelfConfigTarget::ToolServiceRegistry.collection(),
        service_id.to_owned(),
    )];
    super::delete::delete(
        access,
        "operations.mcp_service.delete",
        owner,
        &targets,
        actor,
        digest,
        policy,
    )
    .await
    .map_err(|error| OperationFailure::Operation(McpServiceOperationError::Delete(error)))
}

use gents_protocol::file_inventory::{FileChangeMetadata, FileInventoryEntry};
use gents_protocol::session::SessionScope;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use super::result::{load_tool_file_change_observation, ToolFileChangeRead};

#[derive(Debug, thiserror::Error)]
pub enum InventoryImageError {
    #[error("The recorded file versions do not match the selected inventory.")]
    BindingMismatch,
    #[error(transparent)]
    Read(#[from] anyhow::Error),
}

fn read_scope(
    selected: &SessionScope,
    source: SessionScope,
    binding_matches: bool,
) -> Option<SessionScope> {
    (binding_matches
        && selected
            .requester_did
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && !selected.node_did.is_empty()
        && !selected.session_id.is_empty()
        && !source.session_id.is_empty()
        && selected.node_did == source.node_did
        && selected.requester_did == source.requester_did)
        .then_some(source)
}

pub async fn load_inventory_images(
    access: &ConfigAccess,
    entry: &FileInventoryEntry,
) -> Result<ToolFileChangeRead, InventoryImageError> {
    let response = access.execute_with_variables(
        "query($id: ID!) { AgentToolCall(docID: $id) { _docID request_doc_id node_did session_id requester_did } }",
        &json!({"id": entry.tool_call_doc_id}),
    ).await.map_err(anyhow::Error::from)?;
    let rows = response["data"]["AgentToolCall"]
        .as_array()
        .ok_or(InventoryImageError::BindingMismatch)?;
    let [call] = rows.as_slice() else {
        return Ok(ToolFileChangeRead::Missing);
    };
    let selected = SessionScope {
        node_did: entry.scope.node_did.clone(),
        session_id: entry.scope.session_id.clone(),
        requester_did: Some(entry.scope.requester_did.clone()),
    };
    let source = SessionScope {
        node_did: call["node_did"].as_str().unwrap_or_default().to_owned(),
        session_id: call["session_id"].as_str().unwrap_or_default().to_owned(),
        requester_did: call["requester_did"].as_str().map(str::to_owned),
    };
    let matches = call["_docID"].as_str() == Some(entry.tool_call_doc_id.as_str())
        && call["request_doc_id"].as_str() == Some(entry.request_doc_id.as_str());
    let source =
        read_scope(&selected, source, matches).ok_or(InventoryImageError::BindingMismatch)?;
    if source.session_id != selected.session_id {
        let response = access.execute_with_variables(
            "query($id: ID!) { ToolFileInventory(docID: $id) { _docID tool_call_doc_id observation_doc_id request_doc_id node_did session_id requester_did changes } }",
            &json!({"id": entry.projection_doc_id}),
        ).await.map_err(anyhow::Error::from)?;
        let rows = response["data"]["ToolFileInventory"]
            .as_array()
            .ok_or(InventoryImageError::BindingMismatch)?;
        let [projection] = rows.as_slice() else {
            return Ok(ToolFileChangeRead::Missing);
        };
        if !projection_matches(projection, entry) {
            return Err(InventoryImageError::BindingMismatch);
        }
    }
    let retained = load_tool_file_change_observation(
        access,
        &entry.tool_call_doc_id,
        &entry.request_doc_id,
        &source.node_did,
        &source.session_id,
        source.requester_did.as_deref(),
    )
    .await?;
    if let ToolFileChangeRead::Retained {
        document_id,
        changes,
        ..
    } = &retained
    {
        if document_id != &entry.observation_doc_id
            || changes
                .changes
                .iter()
                .filter(|change| FileChangeMetadata::from(*change) == entry.change)
                .count()
                != 1
        {
            return Err(InventoryImageError::BindingMismatch);
        }
    }
    Ok(retained)
}

fn projection_matches(row: &Value, entry: &FileInventoryEntry) -> bool {
    row["_docID"].as_str() == Some(entry.projection_doc_id.as_str())
        && row["tool_call_doc_id"].as_str() == Some(entry.tool_call_doc_id.as_str())
        && row["observation_doc_id"].as_str() == Some(entry.observation_doc_id.as_str())
        && row["request_doc_id"].as_str() == Some(entry.request_doc_id.as_str())
        && row["node_did"].as_str() == Some(entry.scope.node_did.as_str())
        && row["session_id"].as_str() == Some(entry.scope.session_id.as_str())
        && row["requester_did"].as_str() == Some(entry.scope.requester_did.as_str())
        && row["changes"].as_array().is_some_and(|changes| {
            changes
                .iter()
                .filter(|change| {
                    serde_json::from_value::<FileChangeMetadata>((*change).clone())
                        .ok()
                        .as_ref()
                        == Some(&entry.change)
                })
                .count()
                == 1
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_read_scope_matches_lean() {
        use gents_lean_vocab::families::file_inventory_contracts_image_read_scope as family;
        fn scope(value: &family::Scope) -> SessionScope {
            SessionScope {
                node_did: value.node.clone(),
                session_id: value.session.clone(),
                requester_did: Some(value.requester.clone()),
            }
        }
        family::replay(|input| {
            read_scope(
                &scope(&input.selected),
                scope(&input.source),
                input.binding_matches,
            )
            .map(|scope| family::Scope {
                requester: scope.requester_did.unwrap(),
                node: scope.node_did,
                session: scope.session_id,
            })
        });
    }
}

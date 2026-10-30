use anyhow::{Context, Result};
use base64::Engine as _;
use gents_protocol::file_changes::{
    FileChangeImages, FileEncoding, FileImage, StoredFileChange, StoredFileChanges,
    StoredFileImage, ToolFileChanges, STORED_FILE_CHANGES_VERSION,
};
use gents_protocol::schemas::TOOL_FILE_CONTENT_NAME;
use serde_json::{json, Value};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{document_composite_version, escape_graphql_string, mutation_documents};
use crate::session::origin::SessionScope;

use super::content_hash;

const VERSION: &str = "_version { cid height fieldName }";

/// A file document an observation references, with the scope read back from
/// its immutable key fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDocument {
    pub doc_id: String,
    pub tool_call_doc_id: String,
    pub scope: SessionScope,
}

struct Content {
    doc_id: String,
    text: Option<String>,
    blob: Option<String>,
    version: Option<String>,
}

/// Writes each retained image as a version of the tool observation's file document and
/// returns the observation that names those versions. Runs inside the tool's
/// terminal transaction, so the versions commit only with the observation.
pub(crate) async fn store(
    txn: &ConfigApplyTxn<'_>,
    scope: &SessionScope,
    changes: &ToolFileChanges,
) -> Result<StoredFileChanges> {
    anyhow::ensure!(
        !changes.tool_call_doc_id.is_empty(),
        "file content requires a physical tool identity"
    );
    let mut stored = Vec::with_capacity(changes.changes.len());
    for change in &changes.changes {
        let retains_bytes = [&change.before, &change.after]
            .iter()
            .any(|image| matches!(image, FileImage::File { .. }));
        let mut content = if retains_bytes {
            Some(
                content_document(
                    txn,
                    scope,
                    &changes.tool_call_doc_id,
                    &change.root,
                    &change.path,
                )
                .await?,
            )
        } else {
            None
        };
        let before = store_image(txn, content.as_mut(), &change.before).await?;
        let after = store_image(txn, content.as_mut(), &change.after).await?;
        stored.push(StoredFileChange {
            root: change.root.clone(),
            path: change.path.clone(),
            content_doc_id: content.map(|content| content.doc_id),
            before,
            after,
        });
    }
    Ok(StoredFileChanges {
        version: STORED_FILE_CHANGES_VERSION,
        tool_call_doc_id: changes.tool_call_doc_id.clone(),
        changes: stored,
    })
}

/// A replayed terminal must describe the same observation without writing new
/// versions: paths, hashes, states and metadata agree; versions are not
/// compared.
pub(crate) fn same_observation(stored: &StoredFileChanges, changes: &ToolFileChanges) -> bool {
    stored.tool_call_doc_id == changes.tool_call_doc_id
        && stored.changes.len() == changes.changes.len()
        && stored
            .changes
            .iter()
            .zip(&changes.changes)
            .all(|(stored, change)| {
                stored.root == change.root
                    && stored.path == change.path
                    && same_image(&stored.before, &change.before)
                    && same_image(&stored.after, &change.after)
            })
}

fn same_image(stored: &StoredFileImage, image: &FileImage) -> bool {
    match (stored, image) {
        (StoredFileImage::Missing, FileImage::Missing) => true,
        (StoredFileImage::Unavailable { reason: stored }, FileImage::Unavailable { reason }) => {
            stored == reason
        }
        (
            StoredFileImage::File {
                content_hash: stored_hash,
                permissions: stored_permissions,
                links: stored_links,
                ..
            },
            FileImage::File {
                content_hash,
                permissions,
                links,
                ..
            },
        ) => {
            stored_hash == content_hash
                && stored_permissions == permissions
                && stored_links == links
        }
        _ => false,
    }
}

async fn content_document(
    txn: &ConfigApplyTxn<'_>,
    scope: &SessionScope,
    tool_call_doc_id: &str,
    root: &str,
    path: &str,
) -> Result<Content> {
    let filter = content_filter(scope, tool_call_doc_id, root, path);
    let found = txn
        .execute(&format!(
            "{{ {TOOL_FILE_CONTENT_NAME}(filter: {filter}, limit: 2) {{ _docID text blob {VERSION} }} }}"
        ))
        .await?;
    let rows = found["data"][TOOL_FILE_CONTENT_NAME]
        .as_array()
        .context("file content lookup omitted documents")?;
    match rows.as_slice() {
        [row] => content_from_row(row),
        [] => {
            let created = txn
                .execute_with_variables(
                    &format!(
                        "mutation($input: {TOOL_FILE_CONTENT_NAME}MutationInputArg!) {{ create_{TOOL_FILE_CONTENT_NAME}(input: $input) {{ _docID text blob {VERSION} }} }}"
                    ),
                    &json!({"input": {
                        "node_did": scope.node_did, "session_id": scope.session_id,
                        "requester_did": scope.requester_did, "tool_call_doc_id": tool_call_doc_id, "root": root, "path": path,
                    }}),
                )
                .await?;
            let rows = mutation_documents(&created, &format!("create_{TOOL_FILE_CONTENT_NAME}"))?
                .context("file content document was not created")?;
            anyhow::ensure!(
                rows.len() == 1,
                "file content create returned several documents"
            );
            content_from_row(&rows[0])
        }
        _ => anyhow::bail!("file content has ambiguous documents for one path"),
    }
}

fn content_from_row(row: &Value) -> Result<Content> {
    Ok(Content {
        doc_id: row["_docID"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("file content document has no identity")?
            .to_owned(),
        text: row["text"].as_str().map(str::to_owned),
        blob: row["blob"].as_str().map(str::to_owned),
        version: document_composite_version(row, TOOL_FILE_CONTENT_NAME)?.map(|commit| commit.cid),
    })
}

async fn store_image(
    txn: &ConfigApplyTxn<'_>,
    content: Option<&mut Content>,
    image: &FileImage,
) -> Result<StoredFileImage> {
    let FileImage::File {
        content_hash: hash,
        data,
        permissions,
        links,
    } = image
    else {
        return Ok(match image {
            FileImage::Missing => StoredFileImage::Missing,
            FileImage::Unavailable { reason } => StoredFileImage::Unavailable {
                reason: reason.clone(),
            },
            FileImage::File { .. } => unreachable!(),
        });
    };
    let content = content.context("retained bytes have no file content document")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    anyhow::ensure!(
        content_hash(&bytes) == *hash,
        "file image does not match its content hash"
    );
    let (encoding, field, value) = match String::from_utf8(bytes) {
        Ok(text) => (FileEncoding::Text, "text", text),
        Err(error) => (FileEncoding::Blob, "blob", encode_hex(error.as_bytes())),
    };
    let current = match encoding {
        FileEncoding::Text => &content.text,
        FileEncoding::Blob => &content.blob,
    };
    let version = match (&content.version, current) {
        (Some(version), Some(current)) if *current == value => version.clone(),
        _ => {
            let updated = txn
                .execute_with_variables(
                    &format!(
                        "mutation($doc: ID!, $input: {TOOL_FILE_CONTENT_NAME}MutationInputArg!) {{ update_{TOOL_FILE_CONTENT_NAME}(docID: $doc, input: $input) {{ _docID {VERSION} }} }}"
                    ),
                    &json!({"doc": content.doc_id, "input": {field: value}}),
                )
                .await?;
            let rows = mutation_documents(&updated, &format!("update_{TOOL_FILE_CONTENT_NAME}"))?
                .context("file content version was not written")?;
            anyhow::ensure!(
                rows.len() == 1 && rows[0]["_docID"].as_str() == Some(content.doc_id.as_str()),
                "file content update touched another document"
            );
            let version = document_composite_version(&rows[0], TOOL_FILE_CONTENT_NAME)?
                .context("file content update returned no version")?
                .cid;
            match encoding {
                FileEncoding::Text => content.text = Some(value),
                FileEncoding::Blob => content.blob = Some(value),
            }
            content.version = Some(version.clone());
            version
        }
    };
    Ok(StoredFileImage::File {
        content_hash: hash.clone(),
        version,
        encoding,
        permissions: permissions.clone(),
        links: *links,
    })
}

#[derive(Clone, Copy)]
pub(crate) enum ContentReader<'a, 'txn> {
    Access(&'a ConfigAccess),
    Transaction(&'a ConfigApplyTxn<'txn>),
}

impl ContentReader<'_, '_> {
    async fn execute(self, query: &str) -> Result<Value> {
        match self {
            Self::Access(access) => Ok(access.execute(query).await?),
            Self::Transaction(txn) => Ok(txn.execute(query).await?),
        }
    }
}

pub(crate) async fn decode_observation(
    reader: ContentReader<'_, '_>,
    scope: &SessionScope,
    tool_call_doc_id: &str,
    value: Value,
) -> Result<(ToolFileChanges, Vec<ContentDocument>)> {
    let (value, contents) =
        if value["version"].as_u64() == Some(u64::from(STORED_FILE_CHANGES_VERSION)) {
            let stored = serde_json::from_value(value)
                .context("file image observation has malformed versions")?;
            let (changes, contents) = resolve(reader, scope, stored).await?;
            (serde_json::to_value(changes)?, contents)
        } else {
            (value, Vec::new())
        };
    Ok((
        crate::toolset::FileChanges::decode(tool_call_doc_id, value)?,
        contents,
    ))
}

/// Resolves a stored observation into the images readers receive, reading
/// each version from the file document it names. Every document must carry
/// the observation's own session scope.
pub(crate) async fn resolve(
    access: ContentReader<'_, '_>,
    scope: &SessionScope,
    stored: StoredFileChanges,
) -> Result<(ToolFileChanges, Vec<ContentDocument>)> {
    let mut documents: Vec<ContentDocument> = Vec::new();
    let expected_tool = stored.tool_call_doc_id.clone();
    let mut changes = Vec::with_capacity(stored.changes.len());
    for change in stored.changes {
        let head = match &change.content_doc_id {
            Some(doc_id) => {
                anyhow::ensure!(
                    !documents.iter().any(|document| &document.doc_id == doc_id),
                    "file content document cannot represent multiple paths"
                );
                let (document, head) =
                    content_head(access, doc_id, &change.root, &change.path).await?;
                anyhow::ensure!(
                    document.scope == *scope && document.tool_call_doc_id == expected_tool,
                    "file content crossed its observation scope"
                );
                documents.push(document);
                Some(head)
            }
            None => None,
        };
        changes.push(FileChangeImages {
            before: resolve_image(access, head.as_ref(), change.before).await?,
            after: resolve_image(access, head.as_ref(), change.after).await?,
            root: change.root,
            path: change.path,
        });
    }
    Ok((
        ToolFileChanges {
            version: gents_protocol::file_changes::FILE_CHANGES_VERSION,
            tool_call_doc_id: stored.tool_call_doc_id,
            changes,
        },
        documents,
    ))
}

/// Reads a file document's scope and its stored value. The value stands for a
/// version only while that version is the document's single composite head;
/// concurrent heads store their merge, which matches neither.
async fn content_head(
    access: ContentReader<'_, '_>,
    doc_id: &str,
    root: &str,
    path: &str,
) -> Result<(ContentDocument, Content)> {
    let response = access
        .execute(&format!(
            r#"{{ {TOOL_FILE_CONTENT_NAME}(docID: "{}") {{ _docID node_did session_id requester_did tool_call_doc_id root path text blob _version {{ cid fieldName }} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let rows = response["data"][TOOL_FILE_CONTENT_NAME]
        .as_array()
        .context("file content lookup omitted documents")?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("file content document is unavailable");
    };
    anyhow::ensure!(
        row["_docID"].as_str() == Some(doc_id)
            && row["root"].as_str() == Some(root)
            && row["path"].as_str() == Some(path),
        "file content document names another file"
    );
    let composite: Vec<&str> = row["_version"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|commit| commit["fieldName"].as_str() == Some("_C"))
        .filter_map(|commit| commit["cid"].as_str())
        .collect();
    Ok((
        ContentDocument {
            doc_id: doc_id.to_owned(),
            tool_call_doc_id: row["tool_call_doc_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            scope: SessionScope {
                node_did: row["node_did"].as_str().unwrap_or_default().to_owned(),
                session_id: row["session_id"].as_str().unwrap_or_default().to_owned(),
                requester_did: row["requester_did"].as_str().map(str::to_owned),
            },
        },
        Content {
            doc_id: doc_id.to_owned(),
            text: row["text"].as_str().map(str::to_owned),
            blob: row["blob"].as_str().map(str::to_owned),
            version: match composite.as_slice() {
                [cid] => Some((*cid).to_owned()),
                _ => None,
            },
        },
    ))
}

async fn resolve_image(
    access: ContentReader<'_, '_>,
    head: Option<&Content>,
    image: StoredFileImage,
) -> Result<FileImage> {
    let (hash, version, encoding, permissions, links) = match image {
        StoredFileImage::Missing => return Ok(FileImage::Missing),
        StoredFileImage::Unavailable { reason } => return Ok(FileImage::Unavailable { reason }),
        StoredFileImage::File {
            content_hash,
            version,
            encoding,
            permissions,
            links,
        } => (content_hash, version, encoding, permissions, links),
    };
    let head = head.context("stored file image has no content document")?;
    let (text, blob) = if head.version.as_deref() == Some(version.as_str()) {
        (head.text.clone(), head.blob.clone())
    } else {
        let response = access
            .execute(&format!(
                r#"{{ {TOOL_FILE_CONTENT_NAME}(cid: ["{}"]) {{ _docID text blob }} }}"#,
                escape_graphql_string(&version)
            ))
            .await?;
        let rows = response["data"][TOOL_FILE_CONTENT_NAME]
            .as_array()
            .context("file content version lookup omitted documents")?;
        let [row] = rows.as_slice() else {
            anyhow::bail!("file content version is unavailable");
        };
        anyhow::ensure!(
            row["_docID"].as_str() == Some(head.doc_id.as_str()),
            "file content version belongs to another document"
        );
        (
            row["text"].as_str().map(str::to_owned),
            row["blob"].as_str().map(str::to_owned),
        )
    };
    let bytes = match encoding {
        FileEncoding::Text => text.unwrap_or_default().into_bytes(),
        FileEncoding::Blob => decode_hex(&blob.unwrap_or_default())?,
    };
    anyhow::ensure!(
        content_hash(&bytes) == hash,
        "file content version does not match its content hash"
    );
    Ok(FileImage::File {
        content_hash: hash,
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        permissions,
        links,
    })
}

fn content_filter(scope: &SessionScope, tool_call_doc_id: &str, root: &str, path: &str) -> String {
    let requester = scope
        .requester_did
        .as_deref()
        .map_or("_eq: null".to_owned(), |did| {
            format!(r#"_eq: "{}""#, escape_graphql_string(did))
        });
    format!(
        r#"{{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ {requester} }}, tool_call_doc_id: {{ _eq: "{}" }}, root: {{ _eq: "{}" }}, path: {{ _eq: "{}" }} }}"#,
        escape_graphql_string(&scope.node_did),
        escape_graphql_string(&scope.session_id),
        escape_graphql_string(tool_call_doc_id),
        escape_graphql_string(root),
        escape_graphql_string(path),
    )
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(text.len().is_multiple_of(2), "file content blob is not hex");
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).context("file content blob is not hex"))
        .collect()
}

#[cfg(test)]
#[path = "file_contents/tests.rs"]
mod tests;

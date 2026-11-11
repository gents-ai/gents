use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use anyhow::Context;
use rapidhash::RapidHashMap;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::effect::{DocRef, Row};
use super::hook::Query;
use gents_store::config_client::ConfigRead;
use gents_store::graphql::escape_graphql_string;

/// DefraDB reconstructs at most this many versions of one document per read.
const SNAPSHOT_BATCH: usize = 32;

#[derive(Debug, Deserialize)]
struct DocumentRef {
    #[serde(rename = "_docID")]
    doc_id: String,
}

#[derive(Debug, Deserialize)]
struct CompositeCommit {
    #[serde(default)]
    cid: String,
    #[serde(default)]
    height: i64,
    #[serde(default, rename = "fieldName")]
    field_name: String,
    #[serde(default)]
    heads: Vec<CommitHead>,
    #[serde(default)]
    links: Option<Vec<CommitHead>>,
}

#[derive(Debug, Deserialize)]
struct CommitHead {
    #[serde(default)]
    cid: String,
    #[serde(default, rename = "fieldName")]
    field_name: String,
    #[serde(default)]
    height: Option<i64>,
}

impl CompositeCommit {
    /// A composite commit with one complete parent whose field links are all
    /// complete, all named in `links` and none in `projected` leaves the
    /// projected fields as its parent had them.
    fn inherits(&self, links: &[&str], projected: &[&str]) -> bool {
        let complete = |link: &CommitHead| {
            !link.cid.is_empty() && link.height.is_some_and(|height| height >= 0)
        };
        self.heads.len() == 1
            && self.heads.iter().all(|head| {
                complete(head)
                    && head.field_name == "_C"
                    && head.height.is_some_and(|height| height < self.height)
            })
            && self.links.as_ref().is_some_and(|written| {
                written.iter().all(|link| {
                    complete(link)
                        && links.contains(&link.field_name.as_str())
                        && link.field_name != "_docID"
                        && !projected.contains(&link.field_name.as_str())
                })
            })
    }

    fn parents(&self) -> Vec<String> {
        self.heads
            .iter()
            .filter(|head| head.field_name == "_C")
            .map(|head| head.cid.clone())
            .collect()
    }
}

fn filter(query: &Query) -> String {
    query
        .filter
        .iter()
        .map(|(field, value)| {
            let literal = match value {
                Value::String(text) => format!("\"{}\"", escape_graphql_string(text)),
                other => other.to_string(),
            };
            format!("{field}: {{ _eq: {literal} }}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The revision read a `Query` with `revisions` declares. `collection`,
/// `fields` and filter names were validated as identifiers by the caller.
pub(crate) async fn read<R>(access: &R, query: &Query, links: &str) -> anyhow::Result<Vec<Row>>
where
    R: ConfigRead,
{
    anyhow::ensure!(
        query.order.is_empty() && query.limit.is_none(),
        "a revision read takes no order or limit"
    );
    let collection = query.collection.as_str();
    let response = access
        .execute_read(&format!(
            "{{ {collection}(filter: {{ {} }}, showDeleted: true) {{ _docID }} }}",
            filter(query)
        ))
        .await?;
    let docs: Vec<DocumentRef> = serde_json::from_value(
        response
            .pointer(&format!("/data/{collection}"))
            .cloned()
            .with_context(|| format!("{collection} history query omitted its rows"))?,
    )?;
    let links = links.split_whitespace().collect::<Vec<_>>();
    let projected = query
        .fields
        .split_whitespace()
        .filter(|field| *field != "_docID")
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for doc in docs {
        rows.extend(document_revisions(access, collection, &doc.doc_id, &links, &projected).await?);
    }
    Ok(rows)
}

async fn document_revisions<R>(
    access: &R,
    collection: &str,
    doc_id: &str,
    links: &[&str],
    projected: &[&str],
) -> anyhow::Result<Vec<Row>>
where
    R: ConfigRead,
{
    // Do not send a fieldName filter: DefraDB evaluates that filter in memory
    // and currently degrades malformed filters to no filter.
    let escaped_doc_id = escape_graphql_string(doc_id);
    let response = access
        .execute_read(&format!(
            r#"query {{
                _commits(docID: "{escaped_doc_id}") {{
                    cid
                    height
                    fieldName
                    heads {{ cid height fieldName }}
                    links {{ cid height fieldName }}
                }}
            }}"#
        ))
        .await
        .with_context(|| format!("reading native {collection} history for {doc_id}"))?;
    let commit_values = response
        .pointer("/data/_commits")
        .and_then(Value::as_array)
        .with_context(|| {
            format!("{collection} _commits response carried no data._commits array")
        })?;
    let mut commits = commit_values
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompositeCommit>)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} composite commits"))?
        .into_iter()
        .filter(|commit| commit.field_name == "_C" && !commit.cid.trim().is_empty())
        .collect::<Vec<_>>();
    commits.sort_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid)));
    commits.dedup_by(|left, right| left.cid == right.cid);
    if commits.is_empty() {
        anyhow::bail!(
            "{collection} {doc_id} is visible but its native composite history is unavailable"
        );
    }
    let known = commits
        .iter()
        .map(|commit| commit.cid.as_str())
        .collect::<BTreeSet<_>>();
    for commit in &commits {
        for parent in commit.heads.iter().filter(|head| head.field_name == "_C") {
            if !known.contains(parent.cid.as_str()) {
                anyhow::bail!(
                    "{collection} {doc_id} history is incomplete: composite {} references unavailable parent {}",
                    commit.cid,
                    parent.cid
                );
            }
        }
    }
    let required = commits
        .iter()
        .filter(|commit| !commit.inherits(links, projected))
        .collect::<Vec<_>>();
    let mut snapshots = snapshots(access, collection, doc_id, projected, &required).await?;
    let mut rows: Vec<Row> = Vec::with_capacity(commits.len());
    let mut positions: RapidHashMap<&str, usize> = RapidHashMap::default();
    for commit in &commits {
        let (mut value, inherited) = if let Some(snapshot) = snapshots.remove(&commit.cid) {
            (snapshot, false)
        } else {
            anyhow::ensure!(
                commit.inherits(links, projected),
                "{collection} revision has no native reconstruction"
            );
            let head = &commit.heads[0];
            let parent = &rows[*positions.get(head.cid.as_str()).with_context(|| {
                format!("{collection} history parent is unavailable before projection")
            })?];
            anyhow::ensure!(
                parent
                    .value
                    .pointer("/_revision/height")
                    .and_then(Value::as_i64)
                    == head.height,
                "{collection} history parent metadata disagrees with its native revision"
            );
            (parent.value.clone(), true)
        };
        let object = value
            .as_object_mut()
            .with_context(|| format!("{collection} version is not an object"))?;
        object.insert(
            "_revision".into(),
            serde_json::json!({
                "cid": commit.cid,
                "height": commit.height,
                "parents": commit.parents(),
                "inherited": inherited,
            }),
        );
        let mut hasher = rapidhash::fast::RapidHasher::default();
        value.to_string().hash(&mut hasher);
        positions.insert(commit.cid.as_str(), rows.len());
        rows.push(Row {
            doc: DocRef::new(collection, doc_id),
            revision: hasher.finish(),
            value,
        });
    }
    Ok(rows)
}

async fn snapshots<R>(
    access: &R,
    collection: &str,
    doc_id: &str,
    projected: &[&str],
    commits: &[&CompositeCommit],
) -> anyhow::Result<RapidHashMap<String, Value>>
where
    R: ConfigRead,
{
    let escaped_doc_id = escape_graphql_string(doc_id);
    let fields = projected.join(" ");
    let mut snapshots = RapidHashMap::default();
    for (batch_index, batch) in commits.chunks(SNAPSHOT_BATCH).enumerate() {
        let cid_list = batch
            .iter()
            .map(|commit| format!(r#""{}""#, escape_graphql_string(&commit.cid)))
            .collect::<Vec<_>>()
            .join(", ");
        let response = access
            .execute_read(&format!(
                r#"query {{
            {collection}(cid: [{cid_list}], docID: "{escaped_doc_id}", showDeleted: true) {{ _docID {fields} }}
        }}"#
            ))
            .await
            .with_context(|| {
                format!(
                    "reconstructing native {collection} versions for {doc_id}, batch {} ({} commits)",
                    batch_index + 1,
                    batch.len()
                )
            })?;
        let rows = response
            .pointer(&format!("/data/{collection}"))
            .and_then(Value::as_array)
            .with_context(|| format!("{collection} version query omitted its rows"))?;
        anyhow::ensure!(
            rows.len() == batch.len(),
            "{collection} {doc_id} history batch {} returned {} snapshots for {} visible commits",
            batch_index + 1,
            rows.len(),
            batch.len()
        );
        for (commit, snapshot) in batch.iter().zip(rows) {
            let reconstructed = snapshot.get("_docID").and_then(Value::as_str).unwrap_or("");
            anyhow::ensure!(
                reconstructed == doc_id,
                "{collection} commit {} reconstructed document {}, expected {}",
                commit.cid,
                reconstructed,
                doc_id
            );
            let object: Map<String, Value> = snapshot
                .as_object()
                .cloned()
                .with_context(|| format!("decoding historical {collection} versions"))?;
            snapshots.insert(commit.cid.clone(), Value::Object(object));
        }
    }
    Ok(snapshots)
}

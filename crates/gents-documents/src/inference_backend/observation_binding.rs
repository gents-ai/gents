/// The scope and parent one observation row claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationKey<'a> {
    pub node_did: Option<&'a str>,
    pub backend_id: &'a str,
    pub metadata_doc_id: Option<&'a str>,
}

impl<'a> ObservationKey<'a> {
    pub fn from_row(row: &'a serde_json::Value) -> Option<Self> {
        Some(Self {
            node_did: row["node_did"].as_str(),
            backend_id: row["backend_id"].as_str()?,
            metadata_doc_id: row["metadata_doc_id"].as_str(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ObservationBindingError {
    #[error("backend observation is ambiguous")]
    Ambiguous,
    #[error("backend observation does not match current metadata")]
    Mismatched,
}

/// A backend scope has at most one observation, and it supplies status only
/// while it names the metadata's current physical document. A duplicate or a
/// stale parent binds nothing, so every reader reports the same absence.
pub fn bind<'a, T, I>(
    node_did: &str,
    backend_id: &str,
    metadata_doc_id: &str,
    candidates: I,
) -> Result<Option<T>, ObservationBindingError>
where
    I: IntoIterator<Item = (ObservationKey<'a>, T)>,
{
    let mut scoped = candidates
        .into_iter()
        .filter(|(key, _)| key.node_did == Some(node_did) && key.backend_id == backend_id);
    let Some((key, observation)) = scoped.next() else {
        return Ok(None);
    };
    if scoped.next().is_some() {
        return Err(ObservationBindingError::Ambiguous);
    }
    if key.metadata_doc_id != Some(metadata_doc_id) {
        return Err(ObservationBindingError::Mismatched);
    }
    Ok(Some(observation))
}

/// `bind` over GraphQL rows that select `node_did backend_id metadata_doc_id`.
pub fn bind_rows<'a>(
    node_did: &str,
    backend_id: &str,
    metadata_doc_id: &str,
    rows: &'a [serde_json::Value],
) -> Result<Option<&'a serde_json::Value>, ObservationBindingError> {
    bind(
        node_did,
        backend_id,
        metadata_doc_id,
        rows.iter()
            .filter_map(|row| Some((ObservationKey::from_row(row)?, row))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(node: &'a str, backend: &'a str, parent: Option<&'a str>) -> ObservationKey<'a> {
        ObservationKey {
            node_did: Some(node),
            backend_id: backend,
            metadata_doc_id: parent,
        }
    }

    #[test]
    fn binds_only_a_unique_observation_of_the_current_metadata() {
        assert_eq!(bind::<u8, _>("n", "b", "m", []), Ok(None));
        assert_eq!(
            bind(
                "n",
                "b",
                "m",
                [(key("n", "b", Some("m")), 1), (key("o", "b", Some("m")), 2)]
            ),
            Ok(Some(1))
        );
        assert_eq!(
            bind(
                "n",
                "b",
                "m",
                [(key("n", "b", Some("m")), 1), (key("n", "b", Some("m")), 2)]
            ),
            Err(ObservationBindingError::Ambiguous)
        );
        assert_eq!(
            bind("n", "b", "m", [(key("n", "b", Some("old")), 1)]),
            Err(ObservationBindingError::Mismatched)
        );
        assert_eq!(
            bind("n", "b", "m", [(key("n", "b", None), 1)]),
            Err(ObservationBindingError::Mismatched)
        );
        assert_eq!(
            bind(
                "n",
                "b",
                "m",
                [(
                    ObservationKey {
                        node_did: None,
                        backend_id: "b",
                        metadata_doc_id: Some("m"),
                    },
                    1
                )]
            ),
            Ok(None)
        );
    }
}

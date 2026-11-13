use anyhow::{ensure, Context, Result};
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

/// Missing arrivals are not replaced with content-ID or wall-clock ordering.
pub async fn request_arrival_order(
    txn: &ConfigApplyTxn<'_>,
    doc_ids: &[String],
) -> Result<Vec<String>> {
    Ok(request_arrival_positions(txn, doc_ids)
        .await?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// The native collection-wide cursor lets independently authorized request
/// scans preserve the same arrival order without broadening their actors.
pub(crate) async fn request_arrival_positions(
    txn: &ConfigApplyTxn<'_>,
    doc_ids: &[String],
) -> Result<Vec<(String, u64)>> {
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    const DOC_ID_BATCH_SIZE: usize = 512;
    let mut slots = Vec::new();
    for batch in doc_ids.chunks(DOC_ID_BATCH_SIZE) {
        slots.extend(super::queue::effective_slots(txn, batch).await?);
    }
    if slots.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen_slots = rapidhash::RapidHashSet::default();
    let slot_ids = slots
        .iter()
        .filter_map(|(_, slot)| seen_slots.insert(slot.as_str()).then_some(slot.as_str()))
        .collect::<Vec<_>>();
    let mut ordered = Vec::<(u64, String)>::new();
    for batch in slot_ids.chunks(DOC_ID_BATCH_SIZE) {
        let ids = gents_store::graphql::graphql_string_list_literal(batch.iter().copied());
        let mut cursor = "0".to_owned();
        loop {
            let response = txn.execute(&format!("{{ _documentArrivals(collection: \"AgentRequest\", after: \"{}\", limit: 256, docID: {ids}) {{ head next entries {{ cursor docID }} }} }}", escape_graphql_string(&cursor))).await?;
            let page = &response["data"]["_documentArrivals"];
            let entries = page["entries"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("arrival query omitted entries"))?;
            for entry in entries {
                let arrival = entry["cursor"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("arrival entry omitted cursor"))?
                    .parse::<u64>()
                    .context("arrival entry has invalid cursor")?;
                ordered.push((
                    arrival,
                    entry["docID"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("arrival entry omitted docID"))?
                        .to_owned(),
                ));
            }
            let next = page["next"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("arrival query omitted next cursor"))?;
            let head = page["head"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("arrival query omitted snapshot head"))?;
            if next == head || entries.is_empty() {
                break;
            }
            ensure!(next != cursor, "arrival cursor did not advance");
            cursor = next.into();
        }
    }
    ordered.sort_unstable_by_key(|(cursor, _)| *cursor);
    let positions = ordered
        .iter()
        .map(|(cursor, id)| (id.as_str(), *cursor))
        .collect::<rapidhash::RapidHashMap<_, _>>();
    let mut slots = slots
        .into_iter()
        .filter(|(_, slot)| positions.contains_key(slot.as_str()))
        .collect::<Vec<_>>();
    slots.sort_by_key(|(_, slot)| positions[slot.as_str()]);
    Ok(slots
        .into_iter()
        .map(|(id, slot)| (id, positions[slot.as_str()]))
        .collect())
}

#[derive(Clone, serde::Deserialize)]
pub struct ClaimObservation {
    pub document: String,
    pub owner: String,
    pub session: String,
    pub trigger: String,
    pub serial: bool,
    pub receipt: bool,
    pub arrival: Option<u64>,
    pub running: bool,
    pub terminal: bool,
}

pub(crate) fn claim_conflict(candidate: &ClaimObservation, other: &ClaimObservation) -> bool {
    candidate.owner == other.owner
        && (candidate.session == other.session
            || (candidate.serial
                && candidate.receipt
                && other.receipt
                && candidate.trigger == other.trigger))
}

pub fn observed_claim_allowed(candidate: &ClaimObservation, rows: &[ClaimObservation]) -> bool {
    !candidate.running
        && !candidate.terminal
        && !(candidate.receipt && candidate.arrival.is_none())
        && !rows.iter().any(|other| {
            other.document != candidate.document
                && !other.terminal
                && claim_conflict(candidate, other)
                && ((other.receipt && other.arrival.is_none())
                    || other.running
                    || match (candidate.arrival, other.arrival) {
                        (Some(_), None) => true,
                        (Some(next), Some(prior)) => prior < next,
                        (None, _) => false,
                    })
        })
}

#[cfg(test)]
mod tests {
    use super::{observed_claim_allowed, ClaimObservation};
    use gents_lean_vocab::family::recode;

    #[test]
    fn observed_claim_cohorts_match_lean() {
        gents_lean_vocab::families::trigger_delivery_observed_claim::replay(|case| {
            let candidate: ClaimObservation = recode(&case.candidate);
            let rows: Vec<ClaimObservation> = case.rows.iter().map(recode).collect();
            observed_claim_allowed(&candidate, &rows)
        });
    }
}

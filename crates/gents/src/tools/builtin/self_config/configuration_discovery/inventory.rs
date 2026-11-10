use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

impl Scanner {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        category: DiscoveryCategory,
        display_label: &str,
        state: DiscoveryItemState,
        facts: DiscoveryFacts,
        mapping: MappingSupport,
    ) {
        if self.inventory.items.len() >= self.limits.max_items {
            source.partial = true;
            if !self.item_limit_reported {
                self.item_limit_reported = true;
                self.truncation(
                    DiscoveryNoticeCode::ItemLimit,
                    Some(&source.input.source_id),
                    None,
                    "inventory item limit reached",
                    Some(self.inventory.items.len() as u64 + 1),
                    Some(self.limits.max_items as u64),
                );
            }
            return;
        }
        let label = sanitize_text(display_label);
        let relative_identity = raw_path_string(relative);
        let relative = sanitize_text(&relative_identity);
        let item_id = stable_id(&[
            &source.input.source_id,
            &relative_identity,
            category.as_str(),
            &label,
        ]);
        self.inventory.items.push(DiscoveryItem {
            item_id,
            source_id: source.input.source_id.clone(),
            relative_source_file: relative,
            category,
            display_label: label,
            state,
            facts,
            mapping,
        });
        source.emitted_items += 1;
    }

    pub(super) fn warning(
        &mut self,
        code: DiscoveryNoticeCode,
        source_id: Option<&str>,
        relative: Option<&str>,
        message: &str,
        observed: Option<u64>,
        limit: Option<u64>,
    ) {
        self.record_notice(
            DiscoveryNotice {
                code,
                source_id: source_id.map(sanitize_text),
                relative_source_file: relative.map(sanitize_text),
                safe_message: message.to_owned(),
                observed,
                limit,
            },
            false,
        );
    }

    pub(super) fn truncation(
        &mut self,
        code: DiscoveryNoticeCode,
        source_id: Option<&str>,
        relative: Option<&str>,
        message: &str,
        observed: Option<u64>,
        limit: Option<u64>,
    ) {
        self.record_notice(
            DiscoveryNotice {
                code,
                source_id: source_id.map(sanitize_text),
                relative_source_file: relative.map(sanitize_text),
                safe_message: message.to_owned(),
                observed,
                limit,
            },
            true,
        );
    }

    pub(super) fn record_notice(&mut self, notice: DiscoveryNotice, truncation: bool) {
        let count = self.inventory.warnings.len() + self.inventory.truncation.len();
        if count >= HARD_MAX_NOTICES.saturating_sub(1) {
            if !self.notice_limit_reported {
                self.notice_limit_reported = true;
                self.inventory.truncation.push(DiscoveryNotice {
                    code: DiscoveryNoticeCode::NoticeLimit,
                    source_id: None,
                    relative_source_file: None,
                    safe_message:
                        "additional discovery notices were omitted at the bounded output limit"
                            .to_owned(),
                    observed: Some(count as u64 + 1),
                    limit: Some(HARD_MAX_NOTICES as u64),
                });
            }
            return;
        }
        if truncation {
            self.inventory.truncation.push(notice);
        } else {
            self.inventory.warnings.push(notice);
        }
    }
}

pub(super) fn stable_id(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    let suffix = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("discovery-{suffix}")
}

pub(super) fn detect_conflicts(items: &[DiscoveryItem]) -> Vec<DiscoveryConflict> {
    let mut groups: BTreeMap<(DiscoveryCategory, String), Vec<&DiscoveryItem>> = BTreeMap::new();
    for item in items {
        groups
            .entry((item.category, item.display_label.to_lowercase()))
            .or_default()
            .push(item);
    }
    groups
        .into_iter()
        .filter_map(|((category, label), group)| {
            if group.len() < 2 {
                return None;
            }
            let first = serde_json::to_value((&group[0].state, &group[0].facts)).ok()?;
            let differs = group.iter().skip(1).any(|item| {
                serde_json::to_value((&item.state, &item.facts))
                    .map(|value| value != first)
                    .unwrap_or(true)
            });
            differs.then(|| DiscoveryConflict {
                item_ids: group.iter().map(|item| item.item_id.clone()).collect(),
                explanation: format!(
                    "conflicting {kind} entries named {label:?} were discovered; foreign scope and version precedence differs, so discovery leaves the choice unresolved",
                    kind = category.as_str()
                ),
            })
        })
        .collect()
}

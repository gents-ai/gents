use super::*;
use serde_json::Value;
use gents_fs_runner::beneath::{BeneathError, ChildKind};
use std::path::{Path, PathBuf};

impl Scanner {
    pub(super) fn scan_skill_root(&mut self, source: &mut SourceScan, relative_root: &Path) {
        let mut manifests = Vec::new();
        self.collect_skill_manifests(source, relative_root, 0, &mut manifests);
        manifests.sort();
        for manifest in manifests {
            let Some(content) = self.read_allowlisted(source, &manifest) else {
                continue;
            };
            let parsed = match parse_skill_manifest(&content, &manifest) {
                Ok(parsed) => parsed,
                Err(()) => {
                    source.partial = true;
                    self.warning(
                        DiscoveryNoticeCode::MalformedFile,
                        Some(&source.input.source_id),
                        Some(&path_string(&manifest)),
                        "skill manifest frontmatter was malformed and was discarded",
                        None,
                        None,
                    );
                    continue;
                }
            };
            let state = if source.disabled_skills.contains(&parsed.name) {
                DiscoveryItemState::Disabled
            } else {
                DiscoveryItemState::Enabled
            };
            self.emit(source, &manifest, ItemDraft { category: DiscoveryCategory::Skill, display_label: &parsed.name, state, facts: DiscoveryFacts::Skill {
                    description: parsed.description,
                    untrusted_instructions_excerpt: bounded_sanitized_text(&parsed.body),
                    allowed_tools: parsed.allowed_tools,
                    declared_model: parsed.model,
                }, mapping: MappingSupport::partial(
                    "only the manifest was inspected; supporting files, tool requirements, and activation semantics remain unverified",
                ) });
        }
    }

    pub(super) fn collect_skill_manifests(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        depth: usize,
        manifests: &mut Vec<PathBuf>,
    ) {
        if depth > MAX_SKILL_DEPTH || source.directory_entries >= self.limits.max_directory_entries
        {
            if source.directory_entries >= self.limits.max_directory_entries {
                source.partial = true;
                self.truncation(
                    DiscoveryNoticeCode::DirectoryEntryLimit,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "skill directory entry limit reached",
                    Some(source.directory_entries as u64 + 1),
                    Some(self.limits.max_directory_entries as u64),
                );
            }
            return;
        }
        let directory = match source.held.subdir(relative) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return,
            Err(BeneathError::NotADirectory { .. }) => return,
            Err(BeneathError::Loop { .. } | BeneathError::Outside { .. }) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::SymlinkRejected,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "symlinked skill path was not inspected",
                    None,
                    None,
                );
                return;
            }
            Err(_) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::ReadFailed,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "skill directory could not be inspected",
                    None,
                    None,
                );
                return;
            }
        };
        let entries = match directory.entries() {
            Ok(entries) => entries,
            Err(_) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::ReadFailed,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "skill directory could not be read",
                    None,
                    None,
                );
                return;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            source.directory_entries += 1;
            if source.directory_entries > self.limits.max_directory_entries {
                source.partial = true;
                self.truncation(
                    DiscoveryNoticeCode::DirectoryEntryLimit,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "skill directory entry limit reached",
                    Some(source.directory_entries as u64),
                    Some(self.limits.max_directory_entries as u64),
                );
                break;
            }
            children.push(entry);
        }
        for entry in children {
            let child = relative.join(&entry.name);
            match entry.kind {
                ChildKind::Link => {
                    source.partial = true;
                    self.warning(
                        DiscoveryNoticeCode::SymlinkRejected,
                        Some(&source.input.source_id),
                        Some(&path_string(&child)),
                        "symlinked skill path was not inspected",
                        None,
                        None,
                    );
                }
                ChildKind::Directory => {
                    self.collect_skill_manifests(source, &child, depth + 1, manifests);
                }
                ChildKind::File if entry.name == "SKILL.md" => manifests.push(child),
                ChildKind::File | ChildKind::Other => {}
            }
        }
    }
}

pub(super) struct ParsedSkill {
    name: String,
    description: Option<String>,
    allowed_tools: Vec<String>,
    model: Option<String>,
    body: String,
}

pub(super) fn parse_skill_manifest(content: &str, relative: &Path) -> Result<ParsedSkill, ()> {
    let fallback = relative
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("skill");
    let (frontmatter, body) = split_frontmatter(content)?;
    let object = frontmatter.as_ref().and_then(Value::as_object);
    let name = object
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    let description = object
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .map(bounded_sanitized_text);
    let allowed_tools = object
        .and_then(|value| {
            value
                .get("allowed-tools")
                .or_else(|| value.get("allowed_tools"))
        })
        .map(string_list)
        .unwrap_or_default()
        .into_iter()
        .map(|value| sanitize_text(&value))
        .collect();
    let model = object
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .map(sanitize_text);
    Ok(ParsedSkill {
        name: sanitize_text(name),
        description,
        allowed_tools,
        model,
        body: body.to_owned(),
    })
}

pub(super) fn split_frontmatter(content: &str) -> Result<(Option<Value>, &str), ()> {
    let Some(rest) = content.strip_prefix("---\n") else {
        return Ok((None, content));
    };
    let Some(end) = rest.find("\n---\n") else {
        return Err(());
    };
    let yaml = &rest[..end];
    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|_| ())?;
    let value = serde_json::to_value(parsed).map_err(|_| ())?;
    Ok((Some(value), &rest[end + 5..]))
}

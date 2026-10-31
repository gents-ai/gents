use super::*;
use serde_json::Value;
use std::fs;
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
            self.emit(
                source,
                &manifest,
                DiscoveryCategory::Skill,
                &parsed.name,
                state,
                DiscoveryFacts::Skill {
                    description: parsed.description,
                    untrusted_instructions_excerpt: bounded_sanitized_text(&parsed.body),
                    allowed_tools: parsed.allowed_tools,
                    declared_model: parsed.model,
                },
                MappingSupport::partial(
                    "only the manifest was inspected; supporting files, tool requirements, and activation semantics remain unverified",
                ),
            );
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
        if !source_root_unchanged(source) {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::FileChanged,
                Some(&source.input.source_id),
                Some(&path_string(relative)),
                "selected source root changed during skill inspection",
                None,
                None,
            );
            return;
        }
        let metadata = match reject_symlink_components(&source.root, relative) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return,
            Err(SafePathError::Symlink) => {
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
            Err(SafePathError::Io) => {
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
        if !metadata.is_dir() {
            return;
        }
        let entries = match fs::read_dir(source.root.join(relative)) {
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
        let mut paths = Vec::new();
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
            if let Ok(entry) = entry {
                paths.push(entry.path());
            }
        }
        let directory_stable = reject_symlink_components(&source.root, relative)
            .ok()
            .flatten()
            .is_some_and(|after| metadata_same_file(&metadata, &after));
        if !directory_stable || !source_root_unchanged(source) {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::FileChanged,
                Some(&source.input.source_id),
                Some(&path_string(relative)),
                "skill directory changed during inspection and its entries were discarded",
                None,
                None,
            );
            return;
        }
        paths.sort();
        for path in paths {
            let Ok(child) = path.strip_prefix(&source.root) else {
                continue;
            };
            let child = child.to_path_buf();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                source.partial = true;
                continue;
            };
            if metadata.file_type().is_symlink() {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::SymlinkRejected,
                    Some(&source.input.source_id),
                    Some(&path_string(&child)),
                    "symlinked skill path was not inspected",
                    None,
                    None,
                );
            } else if metadata.is_dir() {
                self.collect_skill_manifests(source, &child, depth + 1, manifests);
            } else if metadata.is_file() && path.file_name().is_some_and(|name| name == "SKILL.md")
            {
                manifests.push(child);
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

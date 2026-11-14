use super::*;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

impl Scanner {
    pub(super) fn scan_source(&mut self, input: DiscoverySourceRoot) {
        if !valid_source_id(&input.source_id) {
            self.warning(
                DiscoveryNoticeCode::InvalidRoot,
                Some(&input.source_id),
                None,
                "source ID must contain only ASCII letters, digits, '.', '_', or '-' and be at most 96 bytes",
                None,
                None,
            );
            self.inventory.sources.push(source_inventory(
                &input,
                DiscoverySourceOutcome::Rejected,
                0,
                0,
                0,
            ));
            return;
        }
        if !input.root.is_absolute() {
            self.warning(
                DiscoveryNoticeCode::RelativeRoot,
                Some(&input.source_id),
                None,
                "selected source root must be absolute",
                None,
                None,
            );
            self.inventory.sources.push(source_inventory(
                &input,
                DiscoverySourceOutcome::Rejected,
                0,
                0,
                0,
            ));
            return;
        }
        let root = match fs::canonicalize(&input.root) {
            Ok(root) if root.is_dir() => root,
            Ok(_) => {
                self.warning(
                    DiscoveryNoticeCode::InvalidRoot,
                    Some(&input.source_id),
                    None,
                    "selected source root is not a directory",
                    None,
                    None,
                );
                self.inventory.sources.push(source_inventory(
                    &input,
                    DiscoverySourceOutcome::Rejected,
                    0,
                    0,
                    0,
                ));
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.warning(
                    DiscoveryNoticeCode::MissingRoot,
                    Some(&input.source_id),
                    None,
                    "selected source root is unavailable",
                    None,
                    None,
                );
                self.inventory.sources.push(source_inventory(
                    &input,
                    DiscoverySourceOutcome::Unavailable,
                    0,
                    0,
                    0,
                ));
                return;
            }
            Err(_) => {
                self.warning(
                    DiscoveryNoticeCode::InvalidRoot,
                    Some(&input.source_id),
                    None,
                    "selected source root could not be resolved",
                    None,
                    None,
                );
                self.inventory.sources.push(source_inventory(
                    &input,
                    DiscoverySourceOutcome::Rejected,
                    0,
                    0,
                    0,
                ));
                return;
            }
        };
        if self
            .containment_root
            .as_ref()
            .is_some_and(|containment_root| !root.starts_with(containment_root))
        {
            self.warning(
                DiscoveryNoticeCode::OutsideContainment,
                Some(&input.source_id),
                None,
                "selected source root resolved outside the authorized tool root",
                None,
                None,
            );
            self.inventory.sources.push(source_inventory(
                &input,
                DiscoverySourceOutcome::Rejected,
                0,
                0,
                0,
            ));
            return;
        }
        let root_metadata = match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            _ => {
                self.warning(
                    DiscoveryNoticeCode::InvalidRoot,
                    Some(&input.source_id),
                    None,
                    "selected source root changed while it was being resolved",
                    None,
                    None,
                );
                self.inventory.sources.push(source_inventory(
                    &input,
                    DiscoverySourceOutcome::Rejected,
                    0,
                    0,
                    0,
                ));
                return;
            }
        };

        let mut source = SourceScan {
            input,
            root,
            root_metadata,
            observed_files: 0,
            observed_bytes: 0,
            emitted_items: 0,
            directory_entries: 0,
            partial: false,
            disabled_skills: BTreeSet::new(),
        };
        let (configs, instructions, skill_roots) = layout(&source.input);
        for (relative, format) in configs {
            if let Some(content) = self.read_allowlisted(&mut source, &relative) {
                self.parse_config(&mut source, &relative, format, &content);
            }
        }
        for relative in instructions {
            if let Some(content) = self.read_allowlisted(&mut source, &relative) {
                let label = relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("instructions");
                self.emit(
                    &mut source,
                    &relative,
                    DiscoveryCategory::Instruction,
                    label,
                    DiscoveryItemState::Enabled,
                    DiscoveryFacts::Instruction {
                        untrusted_excerpt: bounded_sanitized_text(&content),
                        source_bytes: content.len(),
                    },
                    MappingSupport::partial(
                        "instruction excerpt is inert discovery data and requires explicit review before mapping",
                    ),
                );
            }
        }
        for skill_root in skill_roots {
            self.scan_skill_root(&mut source, &skill_root);
        }
        let outcome = if source.partial {
            DiscoverySourceOutcome::Partial
        } else {
            DiscoverySourceOutcome::Complete
        };
        self.inventory.sources.push(source_inventory(
            &source.input,
            outcome,
            source.observed_files,
            source.observed_bytes,
            source.emitted_items,
        ));
    }

    pub(super) fn read_allowlisted(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
    ) -> Option<String> {
        if source.observed_files >= self.limits.max_files_per_source {
            source.partial = true;
            self.truncation(
                DiscoveryNoticeCode::FileLimit,
                Some(&source.input.source_id),
                None,
                "source file limit reached",
                Some((source.observed_files + 1) as u64),
                Some(self.limits.max_files_per_source as u64),
            );
            return None;
        }
        if !safe_relative(relative) {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::InvalidRoot,
                Some(&source.input.source_id),
                None,
                "internal allowlist path was invalid",
                None,
                None,
            );
            return None;
        }
        if !source_root_unchanged(source) {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::FileChanged,
                Some(&source.input.source_id),
                None,
                "selected source root changed during inspection",
                None,
                None,
            );
            return None;
        }
        let path = source.root.join(relative);
        let relative_string = path_string(relative);
        let metadata = match reject_symlink_components(&source.root, relative) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return None,
            Err(SafePathError::Symlink) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::SymlinkRejected,
                    Some(&source.input.source_id),
                    Some(&relative_string),
                    "symlinked allowlisted path was not inspected",
                    None,
                    None,
                );
                return None;
            }
            Err(SafePathError::Io) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::ReadFailed,
                    Some(&source.input.source_id),
                    Some(&relative_string),
                    "allowlisted path could not be inspected",
                    None,
                    None,
                );
                return None;
            }
        };
        if !metadata.is_file() {
            return None;
        }
        source.observed_files += 1;
        if metadata.len() > self.limits.max_file_bytes {
            source.partial = true;
            self.truncation(
                DiscoveryNoticeCode::FileTooLarge,
                Some(&source.input.source_id),
                Some(&relative_string),
                "allowlisted file exceeded the per-file byte limit",
                Some(metadata.len()),
                Some(self.limits.max_file_bytes),
            );
            return None;
        }
        if self.total_bytes.saturating_add(metadata.len()) > self.limits.max_total_bytes {
            source.partial = true;
            self.truncation(
                DiscoveryNoticeCode::TotalBytesLimit,
                Some(&source.input.source_id),
                Some(&relative_string),
                "discovery total byte limit reached",
                Some(self.total_bytes.saturating_add(metadata.len())),
                Some(self.limits.max_total_bytes),
            );
            return None;
        }
        run_preopen_test_hook(&path);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(_) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::ReadFailed,
                    Some(&source.input.source_id),
                    Some(&relative_string),
                    "allowlisted file could not be opened",
                    None,
                    None,
                );
                return None;
            }
        };
        let opened_metadata = match file.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::ReadFailed,
                    Some(&source.input.source_id),
                    Some(&relative_string),
                    "allowlisted path was not a regular file when opened",
                    None,
                    None,
                );
                return None;
            }
        };
        if !metadata_same_file(&metadata, &opened_metadata) {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::FileChanged,
                Some(&source.input.source_id),
                Some(&relative_string),
                "allowlisted file changed before inspection and was discarded",
                None,
                None,
            );
            return None;
        }
        let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
        if file
            .by_ref()
            .take(self.limits.max_file_bytes + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::ReadFailed,
                Some(&source.input.source_id),
                Some(&relative_string),
                "allowlisted file could not be read",
                None,
                None,
            );
            return None;
        }
        let path_metadata = reject_symlink_components(&source.root, relative)
            .ok()
            .flatten();
        let final_metadata = file.metadata().ok();
        if bytes.len() as u64 > self.limits.max_file_bytes
            || !source_root_unchanged(source)
            || path_metadata
                .as_ref()
                .is_none_or(|current| !metadata_same_file(&opened_metadata, current))
            || final_metadata
                .as_ref()
                .is_none_or(|current| !metadata_unchanged(&opened_metadata, current))
        {
            source.partial = true;
            self.warning(
                DiscoveryNoticeCode::FileChanged,
                Some(&source.input.source_id),
                Some(&relative_string),
                "allowlisted file changed during inspection and was discarded",
                None,
                None,
            );
            return None;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::InvalidUtf8,
                    Some(&source.input.source_id),
                    Some(&relative_string),
                    "allowlisted file was not UTF-8 and was discarded",
                    None,
                    None,
                );
                return None;
            }
        };
        self.total_bytes += content.len() as u64;
        source.observed_bytes += content.len() as u64;
        Some(content)
    }
}

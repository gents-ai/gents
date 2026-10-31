use super::*;
use std::collections::BTreeMap;
use std::fs;

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, result: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = fs::read_dir(dir)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if metadata.file_type().is_symlink() {
                result.insert(
                    relative,
                    fs::read_link(path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                );
            } else if metadata.is_dir() {
                walk(root, &path, result);
            } else {
                result.insert(relative, fs::read(path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

fn normalize_source_roots(inventory: &mut ConfigurationDiscoveryInventory, roots: &[(&str, &str)]) {
    for source in &mut inventory.sources {
        source.selected_root = roots
            .iter()
            .find_map(|(source_id, root)| {
                (*source_id == source.source_id).then(|| (*root).to_owned())
            })
            .expect("every fixture source has a stable synthetic root");
    }
}

#[cfg(unix)]
#[test]
fn synthetic_laptop_inventory_is_bounded_inert_and_sanitized() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let user = fixture.path().join("codex-user");
    let project = fixture.path().join("project");
    let process_sentinel = fixture.path().join("HOOK_RAN");
    let user_config =
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &process_sentinel.to_string_lossy());
    write(&user.join("config.toml"), &user_config);
    write(
        &user.join("AGENTS.md"),
        include_str!("../../tests/fixtures/configuration_discovery/codex-user/AGENTS.md"),
    );
    write(
        &user.join("skills/review/SKILL.md"),
        include_str!(
            "../../tests/fixtures/configuration_discovery/codex-user/skills/review/SKILL.md"
        ),
    );
    write(
        &project.join(".codex/config.toml"),
        include_str!("../../tests/fixtures/configuration_discovery/project/.codex/config.toml"),
    );
    write(
        &project.join("AGENTS.md"),
        include_str!("../../tests/fixtures/configuration_discovery/project/AGENTS.md"),
    );
    write(
        &outside.path().join("SKILL.md"),
        "---\nname: escaped\ndescription: FAKE_DISCOVERY_SECRET_123\n---\noutside\n",
    );
    fs::create_dir_all(project.join(".codex/skills")).unwrap();
    symlink(outside.path(), project.join(".codex/skills/escape")).unwrap();

    let before = snapshot(fixture.path());
    let inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![
            DiscoverySourceRoot {
                source_id: "codex-user".to_owned(),
                kind: DiscoverySourceKind::Codex,
                scope: DiscoveryScope::User,
                root: user,
            },
            DiscoverySourceRoot {
                source_id: "codex-project".to_owned(),
                kind: DiscoverySourceKind::Codex,
                scope: DiscoveryScope::Project,
                root: project,
            },
        ],
        limits: DiscoveryLimits::default(),
    });
    let after = snapshot(fixture.path());
    let output = inventory.to_model_json_pretty().unwrap();

    assert_eq!(before, after, "discovery must not mutate selected roots");
    assert!(
        !process_sentinel.exists(),
        "discovery must not execute hooks or MCP commands"
    );
    assert!(!output.contains("FAKE_DISCOVERY_SECRET_123"));
    assert!(
        !output.contains("HOOK_RAN"),
        "raw executable arguments must remain private"
    );
    assert!(
        !output.contains("outside"),
        "escaping symlink contents must not be read"
    );
    assert!(inventory
        .warnings
        .iter()
        .any(|warning| warning.code == DiscoveryNoticeCode::SymlinkRejected));
    assert!(inventory.items.iter().any(|item| {
        item.category == DiscoveryCategory::RemoteTool
            && item.display_label == "shared"
            && item.state == DiscoveryItemState::Disabled
    }));
    assert!(inventory.conflicts.iter().any(|conflict| {
        conflict.item_ids.len() == 2
            && conflict.item_ids.iter().all(|id| {
                inventory
                    .items
                    .iter()
                    .any(|item| &item.item_id == id && item.display_label == "shared")
            })
    }));
    assert_eq!(
        inventory.sources[0].outcome,
        DiscoverySourceOutcome::Complete
    );
    assert_eq!(
        inventory.sources[1].outcome,
        DiscoverySourceOutcome::Partial
    );
}

#[test]
fn malformed_missing_and_limits_are_explicit() {
    let fixture = tempfile::tempdir().unwrap();
    let claude = fixture.path().join("claude");
    fs::create_dir_all(&claude).unwrap();
    write(&claude.join("settings.json"), "{bad");
    write(&claude.join("CLAUDE.md"), "0123456789abcdef");
    let missing = fixture.path().join("missing");
    let inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![
            DiscoverySourceRoot {
                source_id: "claude-user".to_owned(),
                kind: DiscoverySourceKind::Claude,
                scope: DiscoveryScope::User,
                root: claude,
            },
            DiscoverySourceRoot {
                source_id: "grok-user".to_owned(),
                kind: DiscoverySourceKind::Grok,
                scope: DiscoveryScope::User,
                root: missing,
            },
        ],
        limits: DiscoveryLimits {
            max_file_bytes: 8,
            ..DiscoveryLimits::default()
        },
    });
    assert_eq!(
        inventory.sources[0].outcome,
        DiscoverySourceOutcome::Partial
    );
    assert_eq!(
        inventory.sources[1].outcome,
        DiscoverySourceOutcome::Unavailable
    );
    assert!(inventory
        .truncation
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::FileTooLarge));
    assert!(inventory
        .warnings
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::MalformedFile));
    assert!(inventory
        .warnings
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::MissingRoot));
}

#[test]
fn provider_formats_preserve_unsupported_and_disabled_semantics() {
    let fixture = tempfile::tempdir().unwrap();
    let claude = fixture.path().join("claude-project");
    let grok = fixture.path().join("grok-user");
    write(
        &claude.join(".claude/settings.json"),
        r#"{"model":"claude-sonnet","permissions":{"deny":["Bash(rm:*)"]},"hooks":{"PreToolUse":[{"hooks":[{"command":"python /tmp/check.py --token SECRET_VALUE"}]}]},"telemetry":false}"#,
    );
    write(
        &grok.join("config.toml"),
        r#"[models]
default = "grok-build"
[skills]
disabled = ["draft"]
[auth]
api_key = "SECRET_VALUE"
"#,
    );
    write(
        &grok.join("skills/draft/SKILL.md"),
        "---\nname: draft\ndescription: Draft text\n---\nWrite a draft.\n",
    );
    let inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![
            DiscoverySourceRoot {
                source_id: "claude-project".into(),
                kind: DiscoverySourceKind::Claude,
                scope: DiscoveryScope::Project,
                root: claude,
            },
            DiscoverySourceRoot {
                source_id: "grok-user".into(),
                kind: DiscoverySourceKind::Grok,
                scope: DiscoveryScope::User,
                root: grok,
            },
        ],
        limits: DiscoveryLimits::default(),
    });
    let output = inventory.to_model_json_pretty().unwrap();
    assert!(!output.contains("SECRET_VALUE"));
    assert!(inventory
        .items
        .iter()
        .any(|item| item.category == DiscoveryCategory::Hook
            && item.mapping.level == MappingSupportLevel::Unsupported));
    assert!(inventory.items.iter().any(|item| matches!(&item.facts, DiscoveryFacts::UnsupportedSetting { key, .. } if key == "telemetry")));
    assert!(inventory
        .items
        .iter()
        .any(|item| item.category == DiscoveryCategory::Skill
            && item.display_label == "draft"
            && item.state == DiscoveryItemState::Disabled));
}

#[test]
fn ids_are_stable_and_serialization_is_deterministic() {
    let fixture = tempfile::tempdir().unwrap();
    write(
        &fixture.path().join("config.toml"),
        "model = \"gpt-test\"\n",
    );
    let request = DiscoveryRequest {
        sources: vec![DiscoverySourceRoot {
            source_id: "codex-user".into(),
            kind: DiscoverySourceKind::Codex,
            scope: DiscoveryScope::User,
            root: fixture.path().to_path_buf(),
        }],
        limits: DiscoveryLimits::default(),
    };
    let first = discover_configuration(&request);
    let second = discover_configuration(&request);
    assert_eq!(first, second);
    assert_eq!(first.schema_version, 1);
    assert_eq!(first.items.len(), 1);
}

#[cfg(unix)]
#[test]
fn authorized_containment_rejects_a_source_root_symlinked_outside() {
    use std::os::unix::fs::symlink;

    let authorized = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("config.toml"), "model = \"secret\"\n");
    let selected = authorized.path().join("escaped");
    symlink(outside.path(), &selected).unwrap();
    let inventory = discover_configuration_within(
        &DiscoveryRequest {
            sources: vec![DiscoverySourceRoot {
                source_id: "escaped".into(),
                kind: DiscoverySourceKind::Codex,
                scope: DiscoveryScope::User,
                root: selected,
            }],
            limits: DiscoveryLimits::default(),
        },
        &fs::canonicalize(authorized.path()).unwrap(),
    );

    assert!(inventory.items.is_empty());
    assert_eq!(
        inventory.sources[0].outcome,
        DiscoverySourceOutcome::Rejected
    );
    assert!(inventory
        .warnings
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::OutsideContainment));
}

#[cfg(unix)]
#[test]
fn file_replaced_by_symlink_between_check_and_open_is_discarded() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let config = fixture.path().join("config.toml");
    let secret = outside.path().join("config.toml");
    write(&config, "model = \"public\"\n");
    write(&secret, "model = \"RACE_SECRET_MUST_NOT_ESCAPE\"\n");
    PREOPEN_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |checked_path| {
            fs::remove_file(checked_path).unwrap();
            symlink(&secret, checked_path).unwrap();
        }));
    });

    let inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![DiscoverySourceRoot {
            source_id: "raced".into(),
            kind: DiscoverySourceKind::Codex,
            scope: DiscoveryScope::User,
            root: fixture.path().to_path_buf(),
        }],
        limits: DiscoveryLimits::default(),
    });
    let output = inventory.to_model_json_pretty().unwrap();

    assert!(!output.contains("RACE_SECRET_MUST_NOT_ESCAPE"));
    assert!(inventory.items.is_empty());
    assert_eq!(
        inventory.sources[0].outcome,
        DiscoverySourceOutcome::Partial
    );
    assert!(inventory
        .warnings
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::ReadFailed));
}

#[cfg(unix)]
#[test]
fn file_replaced_by_fifo_before_open_does_not_block_discovery() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = tempfile::tempdir().unwrap();
    write(&fixture.path().join("config.toml"), "model = \"public\"\n");
    PREOPEN_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|path| {
            fs::remove_file(path).unwrap();
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: path is a live, NUL-terminated string inside the test home.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        }));
    });
    let inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![DiscoverySourceRoot {
            source_id: "raced".into(),
            kind: DiscoverySourceKind::Codex,
            scope: DiscoveryScope::User,
            root: fixture.path().to_path_buf(),
        }],
        limits: DiscoveryLimits::default(),
    });
    assert!(inventory.items.is_empty());
    assert_eq!(
        inventory.sources[0].outcome,
        DiscoverySourceOutcome::Partial
    );
    assert!(inventory
        .warnings
        .iter()
        .any(|notice| notice.code == DiscoveryNoticeCode::ReadFailed));
}

#[test]
fn redaction_and_model_output_have_explicit_byte_bounds() {
    let redacted = bounded_sanitized_text(
        "\"token\": \"value with spaces\"\npassword = another secret phrase\nBearer abcdefgh\n",
    );
    assert!(!redacted.contains("value with spaces"));
    assert!(!redacted.contains("another secret phrase"));
    assert!(!redacted.contains("abcdefgh"));
    assert!(!redacted.contains("[REDACTED]]"));

    let metadata = sanitize_text(&"x".repeat(MAX_METADATA_FACT_BYTES * 2));
    assert!(metadata.len() <= MAX_METADATA_FACT_BYTES);
    assert!(metadata.ends_with("[TRUNCATED]"));

    let oversized = ConfigurationDiscoveryInventory {
        schema_version: DISCOVERY_SCHEMA_VERSION,
        trust_notice: "x".repeat(MAX_MODEL_INVENTORY_JSON_BYTES),
        sources: vec![],
        items: vec![],
        conflicts: vec![],
        warnings: vec![],
        truncation: vec![],
    };
    assert!(oversized.to_model_json_pretty().is_err());
}

#[test]
fn checked_in_complete_inventory_is_canonical_serializer_output() {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/configuration_discovery");
    let mut inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![
            DiscoverySourceRoot {
                source_id: "codex-user".into(),
                kind: DiscoverySourceKind::Codex,
                scope: DiscoveryScope::User,
                root: fixtures.join("codex-user"),
            },
            DiscoverySourceRoot {
                source_id: "codex-project".into(),
                kind: DiscoverySourceKind::Codex,
                scope: DiscoveryScope::Project,
                root: fixtures.join("project"),
            },
        ],
        limits: DiscoveryLimits::default(),
    });
    normalize_source_roots(
        &mut inventory,
        &[
            ("codex-user", "/synthetic/codex-user"),
            ("codex-project", "/synthetic/project"),
        ],
    );
    let expected =
        include_str!("../../tests/fixtures/configuration_discovery/inventories/complete-v1.json");
    assert_eq!(
        format!("{}\n", inventory.to_model_json_pretty().unwrap()),
        expected
    );
    let decoded: ConfigurationDiscoveryInventory = serde_json::from_str(expected).unwrap();
    assert_eq!(decoded, inventory);
}

#[test]
fn checked_in_partial_inventory_is_canonical_serializer_output() {
    let fixture = tempfile::tempdir().unwrap();
    let claude = fixture.path().join("claude-user");
    fs::create_dir_all(&claude).unwrap();
    write(&claude.join("settings.json"), "{bad");
    write(&claude.join("CLAUDE.md"), "0123456789abcdef");
    let missing = fixture.path().join("missing-grok");
    let mut inventory = discover_configuration(&DiscoveryRequest {
        sources: vec![
            DiscoverySourceRoot {
                source_id: "claude-user".into(),
                kind: DiscoverySourceKind::Claude,
                scope: DiscoveryScope::User,
                root: claude,
            },
            DiscoverySourceRoot {
                source_id: "grok-user".into(),
                kind: DiscoverySourceKind::Grok,
                scope: DiscoveryScope::User,
                root: missing,
            },
        ],
        limits: DiscoveryLimits {
            max_file_bytes: 8,
            ..DiscoveryLimits::default()
        },
    });
    normalize_source_roots(
        &mut inventory,
        &[
            ("claude-user", "/synthetic/claude-user"),
            ("grok-user", "/synthetic/missing-grok"),
        ],
    );
    let expected =
        include_str!("../../tests/fixtures/configuration_discovery/inventories/partial-v1.json");
    assert_eq!(
        format!("{}\n", inventory.to_model_json_pretty().unwrap()),
        expected
    );
    let decoded: ConfigurationDiscoveryInventory = serde_json::from_str(expected).unwrap();
    assert_eq!(decoded, inventory);
}

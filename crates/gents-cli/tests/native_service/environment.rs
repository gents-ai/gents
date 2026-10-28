use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use gents_server::native_service::read_environment_file;

use super::{config, manager, ok, output, NativeServicePlatform, ScriptedRunner};

fn initial(platform: NativeServicePlatform) -> ScriptedRunner {
    ScriptedRunner::with_outputs(match platform {
        NativeServicePlatform::Macos => vec![output(true, "501", ""), ok()],
        NativeServicePlatform::Linux => vec![ok(), ok()],
    })
}

fn stopped_update(platform: NativeServicePlatform) -> ScriptedRunner {
    ScriptedRunner::with_outputs(match platform {
        NativeServicePlatform::Macos => vec![
            output(true, "501", ""),
            output(false, "Could not find service", ""),
            output(true, "501", ""),
            output(true, "{}", ""),
            output(true, "501", ""),
            output(false, "Could not find service", ""),
        ],
        NativeServicePlatform::Linux => vec![
            output(false, "inactive", ""),
            output(false, "disabled", ""),
            ok(),
        ],
    })
}

#[test]
fn reinstall_preserves_operator_environment_when_the_executable_changes() -> Result<()> {
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let previous = config(temp.path(), "old-gents");
        manager(previous.clone(), platform, initial(platform)).install()?;
        let path = previous.definition_path(platform);
        match platform {
            NativeServicePlatform::Macos => {
                let mut plist = plist::Value::from_file(&path)?;
                plist
                    .as_dictionary_mut()
                    .unwrap()
                    .get_mut("EnvironmentVariables")
                    .unwrap()
                    .as_dictionary_mut()
                    .unwrap()
                    .insert("PACK_HTTP_PORT".into(), plist::Value::String("9199".into()));
                plist.to_file_xml(&path)?;
            }
            NativeServicePlatform::Linux => {
                let unit = fs::read_to_string(&path)?.replace(
                    "[Service]\n",
                    "[Service]\nEnvironment=\"PACK_HTTP_PORT=9199\"\n",
                );
                fs::write(&path, unit)?;
            }
        }
        let updated = config(temp.path(), "new-gents");
        manager(updated.clone(), platform, stopped_update(platform)).install()?;
        manager(updated, platform, ScriptedRunner::default()).install()?;
        match platform {
            NativeServicePlatform::Macos => {
                let plist = plist::Value::from_file(&path)?;
                assert_eq!(
                    plist.as_dictionary().unwrap()["EnvironmentVariables"]
                        .as_dictionary()
                        .unwrap()
                        .get("PACK_HTTP_PORT")
                        .and_then(plist::Value::as_string),
                    Some("9199")
                );
            }
            NativeServicePlatform::Linux => assert!(fs::read_to_string(&path)?
                .lines()
                .any(|line| line == "Environment=\"PACK_HTTP_PORT=9199\"")),
        }
    }
    Ok(())
}

#[test]
fn explicit_environment_replacement_updates_and_removes_entries() -> Result<()> {
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let config = config(temp.path(), "gents");
        let input = temp.path().join("environment.json");
        fs::write(&input, r#"{"PACK_KEEP":"old","PACK_REMOVE":"gone"}"#)?;
        manager(config.clone(), platform, initial(platform))
            .install_with_environment(&read_environment_file(&input)?)?;
        fs::write(&input, r#"{"PACK_KEEP":"new"}"#)?;
        manager(config.clone(), platform, stopped_update(platform))
            .install_with_environment(&read_environment_file(&input)?)?;
        let path = config.definition_path(platform);
        let contents = fs::read_to_string(&path)?;
        assert!(!contents.contains("PACK_REMOVE"));
        match platform {
            NativeServicePlatform::Macos => {
                let plist = plist::Value::from_file(&path)?;
                assert_eq!(
                    plist.as_dictionary().unwrap()["EnvironmentVariables"]
                        .as_dictionary()
                        .unwrap()["PACK_KEEP"]
                        .as_string(),
                    Some("new")
                );
            }
            NativeServicePlatform::Linux => assert!(contents
                .lines()
                .any(|line| line == "Environment=\"PACK_KEEP=new\"")),
        }
        manager(config.clone(), platform, ScriptedRunner::default()).install()?;
        fs::write(&input, "{}")?;
        manager(config, platform, stopped_update(platform))
            .install_with_environment(&read_environment_file(&input)?)?;
        let contents = fs::read_to_string(&path)?;
        assert!(!contents.contains("PACK_KEEP"));
        assert!(contents.contains("GENTS_SYSTEM_LOG"));
        assert!(contents.contains("PATH"));
    }
    Ok(())
}

#[test]
fn environment_values_round_trip_without_shell_or_specifier_expansion() -> Result<()> {
    let value = r#"literal$PATH 50% "quoted" \bin & π"#;
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let config = config(temp.path(), "gents");
        manager(config.clone(), platform, initial(platform))
            .install_with_environment(&BTreeMap::from([("PACK_SPECIAL".into(), value.into())]))?;
        manager(config.clone(), platform, ScriptedRunner::default()).install()?;
        match platform {
            NativeServicePlatform::Macos => {
                let plist = plist::Value::from_file(config.definition_path(platform))?;
                assert_eq!(
                    plist.as_dictionary().unwrap()["EnvironmentVariables"]
                        .as_dictionary()
                        .unwrap()["PACK_SPECIAL"]
                        .as_string(),
                    Some(value)
                );
            }
            NativeServicePlatform::Linux => {
                let unit = fs::read_to_string(config.definition_path(platform))?;
                assert!(unit.lines().any(|line| line
                    == r#"Environment="PACK_SPECIAL=literal$PATH 50%% \"quoted\" \\bin & π""#));
            }
        }
    }
    Ok(())
}

#[test]
fn systemd_environment_directives_keep_native_syntax_order_and_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents \"quoted\" \\$%");
    let platform = NativeServicePlatform::Linux;
    manager(config.clone(), platform, initial(platform)).install()?;
    let path = config.definition_path(platform);
    let directives = concat!(
        "Environment=\"PACK_FIRST=%h $literal\" \\\n",
        "# continued comment\n",
        "  'PACK_SECOND=space value'\n",
        "EnvironmentFile=-%h/.config/pack.env\n",
        "PassEnvironment=PACK_INHERITED\n",
        "UnsetEnvironment=PATH GENTS_SYSTEM_LOG PACK_REMOVED\n",
        "Environment=\n",
        "Environment=\"PACK_LAST=after reset\"\n",
        "Environment=PACK_SLASH=\\\\\n",
    );
    let original = fs::read_to_string(&path)?
        .replace("[Unit]\n", "[Unit]\nEnvironment=WRONG_UNIT=1\n")
        .replace("[Service]\n", &format!("[Service]\n{directives}"));
    fs::write(&path, original)?;
    manager(config.clone(), platform, stopped_update(platform)).install()?;
    let updated = fs::read_to_string(&path)?;
    assert!(updated.contains(directives));
    assert!(!updated.contains("WRONG_UNIT"));
    assert_eq!(updated.matches("Type=simple").count(), 1);
    assert_eq!(
        updated
            .matches("Environment=\"GENTS_SYSTEM_LOG=1\"")
            .count(),
        0
    );
    let exec = updated
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .expect("systemd ExecStart");
    assert!(exec.starts_with(
        "\"/usr/bin/env\" -- \"GENTS_SYSTEM_LOG=1\" \"PATH=/reviewed/bin:/usr/bin\" "
    ));
    assert!(exec.contains("\"server\" \"--home\" "));
    assert!(
        !manager(config.clone(), platform, ScriptedRunner::default())
            .installed_executable_differs()?
    );
    let output = std::process::Command::new("/usr/bin/env")
        .env_clear()
        .args([
            "--",
            "GENTS_SYSTEM_LOG=1",
            "PATH=/reviewed/bin:/usr/bin",
            "/usr/bin/env",
        ])
        .env("GENTS_SYSTEM_LOG", "0")
        .env("PATH", "/deliberately-wrong")
        .output()?;
    assert!(output.status.success());
    let environment = String::from_utf8(output.stdout)?;
    assert!(environment.lines().any(|line| line == "GENTS_SYSTEM_LOG=1"));
    assert!(environment
        .lines()
        .any(|line| line == "PATH=/reviewed/bin:/usr/bin"));
    manager(config, platform, ScriptedRunner::default()).install()?;
    assert_eq!(fs::read_to_string(&path)?, updated);
    Ok(())
}

#[test]
fn invalid_environment_input_keeps_values_out_of_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("environment.json");
    for invalid in [
        r#""PRIVATE_TEST_VALUE""#,
        r#"{"PACK_CONFIG":{"PRIVATE_TEST_VALUE":true}}"#,
        r#"{"PACK_CONFIG":"PRIVATE_TEST_VALUE\u0000"}"#,
        r#"{"PACK_CONFIG":"PRIVATE_TEST_VALUE",}"#,
        r#"{"PATH":"PRIVATE_TEST_VALUE"}"#,
        r#"{"GENTS_SYSTEM_LOG":"PRIVATE_TEST_VALUE"}"#,
        r#"{"1BAD_NAME":"PRIVATE_TEST_VALUE"}"#,
    ] {
        fs::write(&file, invalid)?;
        let error = read_environment_file(&file).unwrap_err();
        assert!(!format!("{error:#}").contains("PRIVATE_TEST_VALUE"));
    }
    Ok(())
}

#[test]
fn invalid_replacement_cannot_rewrite_the_installed_definition() -> Result<()> {
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let config = config(temp.path(), "gents");
        manager(config.clone(), platform, initial(platform)).install()?;
        let path = config.definition_path(platform);
        let before = fs::read(&path)?;
        for (key, value, reason) in [
            ("PATH", "PRIVATE_TEST_VALUE", "managed"),
            ("PACK_CONFIG", "PRIVATE_TEST_VALUE\nInjected=1", "control"),
        ] {
            let runner = ScriptedRunner::default();
            let error = manager(config.clone(), platform, runner.clone())
                .install_with_environment(&BTreeMap::from([(key.into(), value.into())]))
                .unwrap_err();
            assert!(error.to_string().contains(reason));
            assert!(!format!("{error:#}").contains("PRIVATE_TEST_VALUE"));
            assert_eq!(fs::read(&path)?, before);
            assert!(runner.calls().is_empty());
        }
    }
    Ok(())
}

#[test]
fn environment_replacement_cannot_take_over_another_home() -> Result<()> {
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let mut config = config(temp.path(), "gents");
        manager(config.clone(), platform, initial(platform)).install()?;
        let path = config.definition_path(platform);
        let before = fs::read(&path)?;
        config.home = temp.path().join("other-home");
        fs::create_dir_all(&config.home)?;
        let runner = ScriptedRunner::default();
        let error = manager(config, platform, runner.clone())
            .install_with_environment(&BTreeMap::new())
            .unwrap_err();
        assert!(error.to_string().contains("belongs to Gents home"));
        assert_eq!(fs::read(&path)?, before);
        assert!(runner.calls().is_empty());
    }
    Ok(())
}

#[test]
fn running_service_refuses_environment_replacement_without_writes() -> Result<()> {
    for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
        let temp = tempfile::tempdir()?;
        let config = config(temp.path(), "gents");
        manager(config.clone(), platform, initial(platform)).install()?;
        let path = config.definition_path(platform);
        let before = fs::read(&path)?;
        let runner = ScriptedRunner::with_outputs(match platform {
            NativeServicePlatform::Macos => vec![
                output(true, "501", ""),
                output(true, "state = running\npid = 44", ""),
                output(true, "501", ""),
                output(true, "{}", ""),
            ],
            NativeServicePlatform::Linux => {
                vec![output(true, "active", ""), output(true, "enabled", "")]
            }
        });
        let service = manager(config, platform, runner.clone());
        let error = service
            .install_with_environment(&BTreeMap::from([("PACK_HTTP_PORT".into(), "9199".into())]))
            .unwrap_err();
        assert!(error.to_string().contains("running"));
        assert_eq!(fs::read(&path)?, before);
        assert!(!runner
            .calls()
            .iter()
            .flatten()
            .any(|arg| matches!(arg.as_str(), "bootout" | "daemon-reload")));
    }
    Ok(())
}

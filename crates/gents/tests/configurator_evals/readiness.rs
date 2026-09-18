use std::path::Path;

pub fn recorded_command(call: &serde_json::Value, root: &Path) -> bool {
    if call["tool_name"] != "bash_unrestricted" || call["lifecycle_state"] != "completed" {
        return false;
    }
    let Some(metadata) = call["result"]
        .as_str()
        .and_then(|result| result.lines().next())
        .and_then(|line| line.strip_prefix("gents_exec: "))
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    else {
        return false;
    };
    if metadata["exit_code"] != 0 || metadata["ok"] != true || metadata["timed_out"] == true {
        return false;
    }
    let Some(argv) = metadata["argv"].as_array() else {
        return false;
    };
    let [shell, argument] = argv.as_slice() else {
        return false;
    };
    let (Some("sh" | "/bin/sh"), Some(argument), Some(cwd)) =
        (shell.as_str(), argument.as_str(), metadata["cwd"].as_str())
    else {
        return false;
    };
    // The process owner records the admitted invocation, not model-authored arguments.
    // Shell programs are deliberately not interpreted as execution evidence.
    !argument.starts_with('-')
        && root
            .join(cwd)
            .join(argument)
            .canonicalize()
            .ok()
            .zip(root.join("readiness/test.sh").canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected)
}

#[test]
fn readiness_uses_process_receipt_not_submitted_arguments_or_stdout() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("readiness")).unwrap();
    std::fs::write(root.path().join("readiness/test.sh"), "# fixture").unwrap();
    for (argv, cwd, accepted) in [
        (serde_json::json!(["sh", "readiness/test.sh"]), ".", true),
        (serde_json::json!(["/bin/sh", "test.sh"]), "readiness", true),
        (
            serde_json::json!(["sh", "-n", "readiness/test.sh"]),
            ".",
            false,
        ),
        (serde_json::json!(["echo", "readiness/test.sh"]), ".", false),
        (
            serde_json::json!(["sh", "-c", "sh readiness/test.sh"]),
            ".",
            false,
        ),
    ] {
        let mut call = serde_json::json!({
            "tool_name":"bash_unrestricted", "lifecycle_state":"completed",
            "args":"deliberately irrelevant",
            "result":format!("gents_exec: {}", serde_json::json!({
                "ok":true,"exit_code":0,"timed_out":false,"argv":argv,"cwd":cwd
            }))
        });
        assert_eq!(recorded_command(&call, root.path()), accepted);
        call["lifecycle_state"] = "failed".into();
        assert!(!recorded_command(&call, root.path()));
    }
    for result in [
        "BUILD_TEST_OK",
        "gents_exec: {\"ok\":true,\"exit_code\":0}",
        "gents_exec: {\"ok\":true,\"exit_code\":0,\"timed_out\":true,\"argv\":[\"sh\",\"readiness/test.sh\"],\"cwd\":\".\"}",
    ] {
        assert!(!recorded_command(&serde_json::json!({
            "tool_name":"bash_unrestricted","lifecycle_state":"completed",
            "args":"{\"command\":\"sh readiness/test.sh\"}","result":result
        }), root.path()));
    }
}

use super::*;

const UUID: &str = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
const REQUEST_ID: &str = "11111111-2222-4333-8444-555555555555";
const SESSION: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn base<'a>() -> HeaderContext<'a> {
    HeaderContext {
        entrypoint: "cli",
        agent_sdk_version: None,
        client_app: None,
        workload: None,
        background: false,
        session_id: SESSION,
        os: "Linux",
        arch: "x64",
        auth: Auth::Bearer("sk-ant-oat01-secret"),
        first_party_base_url: true,
        agent: None,
        agent_type: None,
        request_class: RequestClass::Main,
        prompt_id: None,
        betas: &["claude-code-20250219", "oauth-2025-04-20"],
        client_request_id: REQUEST_ID,
        retry_count: 0,
        timeout_ms: profile::REQUEST_TIMEOUT_MS,
    }
}

fn flat(ctx: &HeaderContext<'_>) -> Vec<(&'static str, String)> {
    request_headers(ctx)
        .into_iter()
        .map(|(name, value)| (name, value.value))
        .collect()
}

fn owned(list: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    list.iter()
        .map(|(name, value)| (*name, (*value).to_owned()))
        .collect()
}

#[test]
fn subscriber_main_thread() {
    let expected = owned(&[
        ("Accept", "application/json"),
        ("X-Stainless-Retry-Count", "0"),
        ("X-Stainless-Timeout", "600"),
        ("X-Stainless-Lang", "js"),
        ("X-Stainless-Package-Version", "0.128.0"),
        ("X-Stainless-OS", "Linux"),
        ("X-Stainless-Arch", "x64"),
        ("X-Stainless-Runtime", "node"),
        ("X-Stainless-Runtime-Version", "v26.12.1"),
        ("anthropic-dangerous-direct-browser-access", "true"),
        ("anthropic-version", "2023-06-01"),
        ("Authorization", "Bearer sk-ant-oat01-secret"),
        ("x-app", "cli"),
        ("User-Agent", "claude-cli/2.1.311 (external, cli)"),
        ("X-Claude-Code-Session-Id", SESSION),
        ("x-claude-code-request-class", "main"),
        ("content-type", "application/json"),
        ("anthropic-beta", "claude-code-20250219,oauth-2025-04-20"),
        ("x-client-request-id", REQUEST_ID),
    ]);
    assert_eq!(flat(&base()), expected);
}

#[test]
fn background_subagent_with_every_hint() {
    let ctx = HeaderContext {
        background: true,
        workload: Some("cron"),
        agent: Some(AgentHeaders {
            agent_id: "agent-1",
            parent_agent_id: Some("parent 2%"),
        }),
        agent_type: Some("Explore"),
        request_class: RequestClass::Subagent,
        prompt_id: Some(UUID),
        retry_count: 2,
        ..base()
    };
    let expected = owned(&[
        ("Accept", "application/json"),
        ("X-Stainless-Retry-Count", "2"),
        ("X-Stainless-Timeout", "600"),
        ("X-Stainless-Lang", "js"),
        ("X-Stainless-Package-Version", "0.128.0"),
        ("X-Stainless-OS", "Linux"),
        ("X-Stainless-Arch", "x64"),
        ("X-Stainless-Runtime", "node"),
        ("X-Stainless-Runtime-Version", "v26.12.1"),
        ("anthropic-dangerous-direct-browser-access", "true"),
        ("anthropic-version", "2023-06-01"),
        ("Authorization", "Bearer sk-ant-oat01-secret"),
        ("x-app", "cli-bg"),
        (
            "User-Agent",
            "claude-cli/2.1.311 (external, cli, workload/cron)",
        ),
        ("X-Claude-Code-Session-Id", SESSION),
        ("x-claude-code-agent-id", "agent-1"),
        ("x-claude-code-parent-agent-id", "parent 2%25"),
        ("x-claude-code-request-class", "subagent"),
        ("x-claude-code-agent-type", "Explore"),
        ("x-claude-code-prompt-id", UUID),
        ("content-type", "application/json"),
        ("anthropic-beta", "claude-code-20250219,oauth-2025-04-20"),
        ("x-client-request-id", REQUEST_ID),
    ]);
    assert_eq!(flat(&ctx), expected);
}

#[test]
fn api_key_replaces_authorization_in_place() {
    let ctx = HeaderContext {
        auth: Auth::ApiKey("sk-ant-api03-secret"),
        ..base()
    };
    let headers = request_headers(&ctx);
    let names: Vec<_> = headers.iter().map(|(name, _)| *name).collect();
    assert_eq!(names[11], "X-Api-Key");
    assert!(!names.contains(&"Authorization"));
    assert_eq!(headers[11].1.value, "sk-ant-api03-secret");
}

#[test]
fn only_credentials_are_sensitive_and_debug_redacts_them() {
    let headers = request_headers(&base());
    let sensitive: Vec<_> = headers
        .iter()
        .filter(|(_, value)| value.sensitive)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(sensitive, ["Authorization"]);
    let rendered = format!("{headers:?} {:?}", base());
    assert!(!rendered.contains("secret"), "{rendered}");
}

#[test]
fn custom_base_url_drops_gated_headers() {
    let ctx = HeaderContext {
        first_party_base_url: false,
        agent: Some(AgentHeaders {
            agent_id: "a",
            parent_agent_id: None,
        }),
        agent_type: Some("Explore"),
        request_class: RequestClass::Subagent,
        prompt_id: Some(UUID),
        ..base()
    };
    let names: Vec<_> = request_headers(&ctx)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(names.contains(&"x-claude-code-agent-id"));
    for gated in [
        "x-claude-code-request-class",
        "x-claude-code-agent-type",
        "x-claude-code-prompt-id",
        "x-client-request-id",
    ] {
        assert!(!names.contains(&gated), "{gated}");
    }
    assert_eq!(names.last(), Some(&"anthropic-beta"));
}

#[test]
fn optional_headers_are_omitted_when_empty_or_invalid() {
    let ctx = HeaderContext {
        prompt_id: Some("not-a-uuid"),
        betas: &[],
        timeout_ms: 0,
        agent: Some(AgentHeaders {
            agent_id: "",
            parent_agent_id: Some("p"),
        }),
        agent_type: Some(""),
        client_app: Some(""),
        ..base()
    };
    let names: Vec<_> = request_headers(&ctx)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for absent in [
        "x-claude-code-prompt-id",
        "anthropic-beta",
        "X-Stainless-Timeout",
        "x-claude-code-agent-id",
        "x-claude-code-parent-agent-id",
        "x-claude-code-agent-type",
        "x-client-app",
    ] {
        assert!(!names.contains(&absent), "{absent}");
    }
}

#[test]
fn client_app_is_sent_after_the_session_id() {
    let ctx = HeaderContext {
        client_app: Some("my-app"),
        ..base()
    };
    let headers = flat(&ctx);
    assert_eq!(
        headers[13].1,
        "claude-cli/2.1.311 (external, cli, client-app/my-app)"
    );
    assert_eq!(headers[15], ("x-client-app", "my-app".to_owned()));
}

#[test]
fn sub_second_timeouts_truncate() {
    let ctx = HeaderContext {
        timeout_ms: 1999,
        ..base()
    };
    assert!(flat(&ctx).contains(&("X-Stainless-Timeout", "1".to_owned())));
}

#[test]
fn user_agent_variants() {
    assert_eq!(
        user_agent("cli", None, None, None),
        "claude-cli/2.1.311 (external, cli)"
    );
    assert_eq!(
        user_agent("sdk-ts", Some("0.2.0"), None, None),
        "claude-cli/2.1.311 (external, sdk-ts, agent-sdk/0.2.0)"
    );
    assert_eq!(
        user_agent("sdk-py", Some("1.0"), Some("desk"), Some("cron")),
        "claude-cli/2.1.311 (external, sdk-py, agent-sdk/1.0, client-app/desk, workload/cron)"
    );
    assert_eq!(
        user_agent("cli", Some(""), Some(""), Some("")),
        "claude-cli/2.1.311 (external, cli)"
    );
}

#[test]
fn os_mapping() {
    for (input, expected) in [
        ("darwin", "MacOS"),
        ("Darwin", "MacOS"),
        ("win32", "Windows"),
        ("linux", "Linux"),
        ("freebsd", "FreeBSD"),
        ("openbsd", "OpenBSD"),
        ("android", "Android"),
        ("ios", "iOS"),
        ("iPadOS-ios", "iOS"),
        ("sunos", "Other:sunos"),
        ("AIX", "Other:aix"),
        ("", "Unknown"),
    ] {
        assert_eq!(platform_os(input), expected, "{input}");
    }
}

#[test]
fn arch_mapping() {
    for (input, expected) in [
        ("x32", "x32"),
        ("x64", "x64"),
        ("x86_64", "x64"),
        ("arm", "arm"),
        ("arm64", "arm64"),
        ("aarch64", "arm64"),
        ("ia32", "other:ia32"),
        ("riscv64", "other:riscv64"),
        ("", "unknown"),
    ] {
        assert_eq!(platform_arch(input), expected, "{input}");
    }
}

#[test]
fn host_platform_is_a_node_name() {
    let (os, arch) = host_platform();
    assert!(!os.is_empty() && !arch.is_empty());
    assert!(
        !["macos", "windows", "x86_64", "aarch64"].contains(&os)
            && !["x86_64", "aarch64", "x86"].contains(&arch)
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!((os, arch), ("linux", "x64"));
}

#[test]
fn agent_values_are_percent_encoded() {
    assert_eq!(escape_agent_value("plain-id_1 ~"), "plain-id_1 ~");
    assert_eq!(escape_agent_value("100%"), "100%25");
    assert_eq!(escape_agent_value("a\tb\u{7f}"), "a%09b%7F");
    assert_eq!(escape_agent_value("é"), "%C3%A9");
    assert_eq!(escape_agent_value("😀"), "%F0%9F%98%80");
}

#[test]
fn prompt_ids_must_be_uuids() {
    assert!(is_uuid(UUID));
    assert!(is_uuid(&UUID.to_uppercase()));
    for bad in [
        "",
        "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4",
        "0a1b2c3d04e5f-4a6b-8c7d-9e0f1a2b3c4d",
        "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4g",
    ] {
        assert!(!is_uuid(bad), "{bad}");
    }
}

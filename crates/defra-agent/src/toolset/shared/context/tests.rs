use super::resolve_default_read_root;

#[test]
fn default_read_root_prefers_current_dir() {
    let cwd = std::env::temp_dir().join("defra-agent-cwd-root");
    let home = std::env::temp_dir().join("defra-agent-home-root");
    let resolved = resolve_default_read_root(Some(cwd.clone()), Some(home)).unwrap();
    assert_eq!(resolved, cwd);
}

#[test]
fn default_read_root_falls_back_to_home() {
    let home = std::env::temp_dir().join("defra-agent-home-root");
    let resolved = resolve_default_read_root(None, Some(home.clone())).unwrap();
    assert_eq!(resolved, home);
}

#[test]
fn default_read_root_errors_when_unavailable() {
    assert!(resolve_default_read_root(None, None).is_err());
}

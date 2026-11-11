use gents_documents::is_reserved_builtin_tool_name;

#[test]
fn reserved_names_cover_native_and_meta_tools() {
    use crate::native::NativeTool;

    let native = [
        NativeTool::ListFiles { max_entries: 1 },
        NativeTool::ReadFile { max_chars: 1 },
        NativeTool::Glob { max_matches: 1 },
        NativeTool::Grep { max_matches: 1 },
        NativeTool::WriteFile {
            root: std::path::PathBuf::from("/tmp"),
        },
        NativeTool::EditFile {
            root: std::path::PathBuf::from("/tmp"),
        },
    ];
    for tool in &native {
        assert!(
            gents_protocol::tool_names::NATIVE_TOOL_NAMES.contains(&tool.tool_name().as_str()),
            "native tool {:?} missing from NATIVE_TOOL_NAMES",
            tool.tool_name()
        );
        assert!(
            is_reserved_builtin_tool_name(&tool.tool_name()),
            "native tool {:?} must be reserved",
            tool.tool_name()
        );
    }
    for name in gents_protocol::tool_names::NATIVE_TOOL_NAMES {
        assert!(
            is_reserved_builtin_tool_name(name),
            "registry name {name:?} must be reserved"
        );
    }
    assert!(is_reserved_builtin_tool_name("bash"));
    assert!(is_reserved_builtin_tool_name("bash_unrestricted"));
    assert!(is_reserved_builtin_tool_name("lsp"));

    for meta in gents_protocol::tool_names::META_TOOL_NAMES {
        assert!(
            is_reserved_builtin_tool_name(meta),
            "meta tool {meta:?} must be reserved"
        );
    }
    for goal in [
        gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
        gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
    ] {
        assert!(
            is_reserved_builtin_tool_name(goal),
            "goal tool {goal:?} must be reserved"
        );
    }

    assert!(!is_reserved_builtin_tool_name("request_action"));
    assert!(!is_reserved_builtin_tool_name("memory"));
}

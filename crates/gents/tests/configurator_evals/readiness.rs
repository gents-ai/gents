use std::path::Path;
use tree_sitter::{Node, Parser};

fn invokes_script(argv: &[String], cwd: &Path, script: &Path) -> bool {
    let [shell, argument] = argv else {
        return false;
    };
    matches!(shell.as_str(), "sh" | "/bin/sh")
        && !argument.starts_with('-')
        && cwd
            .join(argument)
            .canonicalize()
            .ok()
            .zip(script.canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected)
}

fn safe_expansions(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "command_substitution"
        | "process_substitution"
        | "variable_assignment"
        | "arithmetic_expansion"
        | "expansion" => return false,
        "simple_expansion" if &source[node.byte_range()] != "$?" => return false,
        "heredoc_redirect" => {
            // Only literal heredocs: their contents are data, never invocation evidence.
            let text = &source[node.byte_range()];
            return text.trim_start().starts_with("<<'");
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let safe = node
        .named_children(&mut cursor)
        .all(|child| safe_expansions(child, source));
    safe
}

// A successful receipt proves the final && chain ran, not earlier statements.
fn visit(node: Node<'_>, source: &str, cwd: &Path, script: &Path) -> Option<bool> {
    match node.kind() {
        "comment" => Some(false),
        "program" | "list" => {
            let mut found = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    if child.kind() == "comment" {
                        continue;
                    }
                    let invoked = visit(child, source, cwd, script)?;
                    if node.kind() == "program" {
                        found = invoked;
                    } else {
                        found |= invoked;
                    }
                } else if child.kind() == ";" && node.kind() == "program" {
                    continue;
                } else if child.kind() != "&&" {
                    return None;
                }
            }
            Some(found)
        }
        "redirected_statement" => {
            let body = node.child_by_field_name("body")?;
            let mut cursor = node.walk();
            if node.named_children(&mut cursor).any(|child| {
                child.id() != body.id()
                    && child.kind() != "heredoc_redirect"
                    && child.kind() != "file_redirect"
            }) {
                return None;
            }
            visit(body, source, cwd, script)
        }
        "command" => {
            let name = node.child_by_field_name("name")?;
            let name = shlex::split(&source[name.byte_range()])?;
            let [name] = name.as_slice() else { return None };
            if matches!(name.as_str(), "sh" | "/bin/sh") {
                let argv = shlex::split(&source[node.byte_range()])?;
                return invokes_script(&argv, cwd, script).then_some(true);
            }
            matches!(name.as_str(), "mkdir" | "chmod" | "cat" | "echo" | "true").then_some(false)
        }
        _ => None,
    }
}

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
    let Some(args) = call["args"]
        .as_str()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    else {
        return false;
    };
    let Some(command) = args["command"].as_str() else {
        return false;
    };
    let cwd = root.join(args["cwd"].as_str().unwrap_or("."));
    let script = root.join("readiness/test.sh");
    let program;
    if let Some(extra) = args["args"].as_array().filter(|args| !args.is_empty()) {
        let Some(argv) = std::iter::once(Some(command.to_owned()))
            .chain(extra.iter().map(|arg| arg.as_str().map(str::to_owned)))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        if invokes_script(&argv, &cwd, &script) {
            return true;
        }
        let [shell, flag, body] = argv.as_slice() else {
            return false;
        };
        if !matches!(shell.as_str(), "sh" | "/bin/sh") || !matches!(flag.as_str(), "-c" | "-lc") {
            return false;
        }
        program = body.clone();
    } else {
        program = command.to_owned();
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("Bash grammar");
    let Some(tree) = parser.parse(&program, None) else {
        return false;
    };
    let node = tree.root_node();
    !node.has_error()
        && safe_expansions(node, &program)
        && visit(node, &program, &cwd, &script) == Some(true)
}

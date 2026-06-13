pub(crate) const DEPRECATED: &[(&[&str], &str)] = &[
    (&["config", "task"], "task"),
    (&["p2p", "unpair"], "p2p pairings rm"),
    (&["p2p", "pairings", "remove"], "p2p pairings rm"),
    (&["show", "request"], "request show"),
    (&["show", "response"], "response show"),
];

pub(crate) fn deprecation_warning(argv: &[String]) -> Option<String> {
    let words = command_words(argv);
    DEPRECATED.iter().find_map(|(path, replacement)| {
        words.starts_with(path).then(|| {
            format!(
                "warning: `defra-agent {}` is deprecated; use `defra-agent {replacement}`",
                path.join(" ")
            )
        })
    })
}

fn command_words(argv: &[String]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut iter = argv.iter().skip(1).peekable();

    while let Some(arg) = iter.next() {
        let s = arg.as_str();
        if s == "--" {
            break;
        }
        if s.starts_with("--") {
            if !s.contains('=') {
                if let Some(next) = iter.peek() {
                    if !next.starts_with('-') {
                        iter.next();
                    }
                }
            }
            continue;
        }
        if s.starts_with('-') {
            continue;
        }
        words.push(s);
        while let Some(arg) = iter.next() {
            let s = arg.as_str();
            if s == "--" {
                break;
            }
            if s.starts_with("--") {
                if !s.contains('=') {
                    if let Some(next) = iter.peek() {
                        if !next.starts_with('-') {
                            iter.next();
                        }
                    }
                }
                continue;
            }
            if s.starts_with('-') {
                continue;
            }
            words.push(s);
        }
        break;
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn config_task_warns() {
        let warning = deprecation_warning(&argv(&["defra-agent", "config", "task", "run", "x"]))
            .expect("expected config task warning");
        assert_eq!(
            warning,
            "warning: `defra-agent config task` is deprecated; use `defra-agent task`"
        );
    }

    #[test]
    fn replacement_task_path_does_not_warn() {
        assert_eq!(
            deprecation_warning(&argv(&["defra-agent", "task", "run", "x"])),
            None
        );
    }

    #[test]
    fn leading_option_values_are_skipped() {
        let warning = deprecation_warning(&argv(&[
            "defra-agent",
            "--home",
            "local-home",
            "config",
            "task",
        ]))
        .expect("expected warning after leading option");
        assert_eq!(
            warning,
            "warning: `defra-agent config task` is deprecated; use `defra-agent task`"
        );
    }

    #[test]
    fn leading_option_equals_form_is_skipped() {
        let warning = deprecation_warning(&argv(&[
            "defra-agent",
            "--home=local-home",
            "show",
            "response",
            "req-1",
        ]))
        .expect("expected warning after leading option");
        assert_eq!(
            warning,
            "warning: `defra-agent show response` is deprecated; use `defra-agent response show`"
        );
    }

    #[test]
    fn unknown_commands_do_not_warn() {
        assert_eq!(
            deprecation_warning(&argv(&["defra-agent", "config", "backend", "set"])),
            None
        );
    }

    #[test]
    fn p2p_alias_paths_warn() {
        assert_eq!(
            deprecation_warning(&argv(&["defra-agent", "p2p", "unpair", "--peer", "peer-1"])),
            Some(
                "warning: `defra-agent p2p unpair` is deprecated; use `defra-agent p2p pairings rm`"
                    .to_string()
            )
        );
        assert_eq!(
            deprecation_warning(&argv(&[
                "defra-agent",
                "p2p",
                "pairings",
                "remove",
                "--peer",
                "peer-1",
            ])),
            Some(
                "warning: `defra-agent p2p pairings remove` is deprecated; use `defra-agent p2p pairings rm`"
                    .to_string()
            )
        );
    }

    #[test]
    fn trailing_option_values_are_skipped() {
        assert_eq!(
            command_words(&argv(&[
                "defra-agent",
                "p2p",
                "pairings",
                "remove",
                "--peer",
                "peer-1",
            ])),
            vec!["p2p", "pairings", "remove"]
        );
    }
}

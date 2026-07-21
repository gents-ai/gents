pub(crate) const DEPRECATED: &[(&[&str], &str)] = &[
    (&["config", "task"], "task"),
    (&["show", "request"], "request show"),
    (&["show", "response"], "response show"),
    (&["p2p", "pair"], "p2p pairings set"),
    (&["p2p", "unpair"], "p2p pairings rm"),
];

pub(crate) fn deprecation_warning(argv: &[String]) -> Option<String> {
    let words = command_words(argv);
    DEPRECATED.iter().find_map(|(path, replacement)| {
        words.starts_with(path).then(|| {
            format!(
                "warning: `gents {}` is deprecated; use `gents {replacement}`",
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
        let warning = deprecation_warning(&argv(&["gents", "config", "task", "run", "x"]))
            .expect("expected config task warning");
        assert_eq!(
            warning,
            "warning: `gents config task` is deprecated; use `gents task`"
        );
    }

    #[test]
    fn replacement_task_path_does_not_warn() {
        assert_eq!(
            deprecation_warning(&argv(&["gents", "task", "run", "x"])),
            None
        );
    }

    #[test]
    fn leading_option_values_are_skipped() {
        let warning = deprecation_warning(&argv(&[
            "gents",
            "--home",
            "local-home",
            "config",
            "task",
        ]))
        .expect("expected warning after leading option");
        assert_eq!(
            warning,
            "warning: `gents config task` is deprecated; use `gents task`"
        );
    }

    #[test]
    fn leading_option_equals_form_is_skipped() {
        let warning = deprecation_warning(&argv(&[
            "gents",
            "--home=local-home",
            "show",
            "response",
            "req-1",
        ]))
        .expect("expected warning after leading option");
        assert_eq!(
            warning,
            "warning: `gents show response` is deprecated; use `gents response show`"
        );
    }

    #[test]
    fn unknown_commands_do_not_warn() {
        assert_eq!(
            deprecation_warning(&argv(&["gents", "config", "backend", "set"])),
            None
        );
    }

    #[test]
    fn p2p_pair_warns() {
        let warning =
            deprecation_warning(&argv(&["gents", "p2p", "pair", "--peer", "peer-1"]))
                .expect("expected p2p pair warning");
        assert_eq!(
            warning,
            "warning: `gents p2p pair` is deprecated; use `gents p2p pairings set`"
        );
    }

    #[test]
    fn p2p_unpair_warns() {
        let warning =
            deprecation_warning(&argv(&["gents", "p2p", "unpair", "--peer", "peer-1"]))
                .expect("expected p2p unpair warning");
        assert_eq!(
            warning,
            "warning: `gents p2p unpair` is deprecated; use `gents p2p pairings rm`"
        );
    }

    #[test]
    fn pairings_alias_paths_do_not_warn() {
        // `p2p pairings unpair` is a blessed alias of `rm`, not a deprecated
        // spelling — it must parse silently.
        assert_eq!(
            deprecation_warning(&argv(&[
                "gents",
                "p2p",
                "pairings",
                "unpair",
                "--peer",
                "peer-1",
            ])),
            None
        );
    }

    #[test]
    fn trailing_option_values_are_skipped() {
        assert_eq!(
            command_words(&argv(&[
                "gents",
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

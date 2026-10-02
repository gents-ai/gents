//! The chat's answer to "Allow this plugin to read this path?" (see
//! `gents::plugin::approval`): asked on the terminal between turn polls, so
//! the call waiting in the server continues as soon as the operator answers.

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::Result;
use gents::plugin::approval::{self, Answer, Request};

/// `o` allows once, `f` always this file, `a` always the folder, anything
/// else (an empty line included) denies.
pub(super) fn parse_answer(line: &str) -> Answer {
    match line.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => Answer::Once,
        "f" | "file" => Answer::AlwaysPath,
        "a" | "always" | "folder" => Answer::AlwaysFolder,
        _ => Answer::Deny,
    }
}

fn mine(request: &Request, session_id: &str) -> bool {
    request
        .session_id
        .as_deref()
        .is_none_or(|id| id == session_id)
}

/// Whether `session_id` has a question waiting.
pub(super) fn has_pending(home: &Path, session_id: &str) -> Result<bool> {
    Ok(approval::pending(home)?
        .iter()
        .any(|request| mine(request, session_id)))
}

/// Asks `read` about every question `session_id` is waiting on and writes
/// the answers; true when any was asked.
pub(super) fn answer_pending(
    home: &Path,
    session_id: &str,
    mut read: impl FnMut(&Request) -> Answer,
) -> Result<bool> {
    let mut asked = false;
    for request in approval::pending(home)? {
        if !mine(&request, session_id) {
            continue;
        }
        asked = true;
        let answer = read(&request);
        approval::decide(home, &request.id, answer)?;
    }
    Ok(asked)
}

/// The terminal reader: prints the question and reads one line. Without a
/// terminal there is nobody to ask, so the call is denied.
pub(super) fn ask_on_terminal(request: &Request) -> Answer {
    if !std::io::stdin().is_terminal() {
        eprintln!("{} No terminal to ask on; denied.", request.prompt());
        return Answer::Deny;
    }
    println!("{}", request.prompt());
    let file = if request.is_dir {
        String::new()
    } else {
        "  [f] always allow this file".to_owned()
    };
    print!(
        "[o] allow once{file}  [a] always allow the folder {}  [d] deny (default) > ",
        request.folder.display()
    );
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(_) => parse_answer(&line),
        Err(_) => Answer::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::pack::BindAccess;
    use gents::plugin::allowed::{self, Resolved};

    #[test]
    fn answers_parse_and_default_to_deny() {
        assert_eq!(parse_answer("o\n"), Answer::Once);
        assert_eq!(parse_answer(" Always "), Answer::AlwaysFolder);
        assert_eq!(parse_answer("f"), Answer::AlwaysPath);
        assert_eq!(parse_answer("yes"), Answer::Once);
        assert_eq!(parse_answer(""), Answer::Deny);
        assert_eq!(parse_answer("whatever"), Answer::Deny);
    }

    #[test]
    fn only_this_sessions_questions_are_answered_and_always_stores_the_folder() {
        let home = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let resolved = Resolved {
            target: folder.path().canonicalize().unwrap().join("x.pdf"),
            is_dir: false,
        };
        let mine = Request::new("team/ocr", &resolved, BindAccess::Read, Some("s1".into()));
        let other = Request::new("team/ocr", &resolved, BindAccess::Read, Some("s2".into()));
        for request in [&mine, &other] {
            let dir = home.path().join(gents::home::PLUGIN_APPROVALS_DIR_NAME);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{}.json", request.id)),
                serde_json::to_vec(request).unwrap(),
            )
            .unwrap();
        }
        let mut seen = Vec::new();
        let asked = answer_pending(home.path(), "s1", |request| {
            seen.push(request.id.clone());
            Answer::AlwaysFolder
        })
        .unwrap();
        assert!(asked);
        assert_eq!(seen, vec![mine.id.clone()]);
        assert_eq!(approval::pending(home.path()).unwrap(), vec![other]);
        let listed = allowed::list(home.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, folder.path().canonicalize().unwrap());
        assert!(!answer_pending(home.path(), "s1", |_| Answer::Deny).unwrap());
    }
}

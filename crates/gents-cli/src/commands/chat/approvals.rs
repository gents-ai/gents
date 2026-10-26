use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::Result;
use gents::plugin::approval::{self, Answer, Request};

pub(super) fn parse_answer(line: &str) -> Answer {
    match line.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" => Answer::Once,
        "c" | "command" | "invocation" => Answer::AlwaysInvocation,
        "f" | "file" => Answer::AlwaysPath,
        "a" | "always" | "folder" => Answer::AlwaysFolder,
        _ => Answer::Deny,
    }
}

pub(super) fn mine(request: &Request, session_id: &str) -> bool {
    request
        .session_id
        .as_deref()
        .is_none_or(|id| id == session_id)
}

pub(super) fn has_pending(home: &Path, session_id: &str) -> Result<bool> {
    Ok(approval::pending(home)?
        .iter()
        .any(|request| mine(request, session_id)))
}

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

pub(super) fn ask_on_terminal(request: &Request) -> Answer {
    if !std::io::stdin().is_terminal() {
        eprintln!("{} No terminal to ask on; denied.", request.prompt());
        return Answer::Deny;
    }
    show_prompt(request);
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(_) => parse_answer(&line),
        Err(_) => Answer::Deny,
    }
}

pub(super) fn show_prompt(request: &Request) {
    let mut output = std::io::stdout().lock();
    let _ = writeln!(output, "{}", request.prompt());
    let _ = write!(output, "[o] allow once");
    if let Some(file) = request.filesystem() {
        if request.allows_answer(Answer::AlwaysPath) && !file.is_dir {
            let _ = write!(output, "  [f] always allow this file");
        }
        if request.allows_answer(Answer::AlwaysFolder) {
            let _ = write!(
                output,
                "  [a] always allow the folder {}",
                file.folder.display()
            );
        }
    }
    if request.allows_answer(Answer::AlwaysInvocation) {
        let _ = write!(
            output,
            "  [c] remember this exact command until permissions change"
        );
    }
    let _ = write!(output, "  [d] deny (default) > ");
    let _ = output.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::pack::BindAccess;
    use gents::plugin::allowed::{self, Resolved};

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
            let dir = approval::queue_dir(home.path());
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

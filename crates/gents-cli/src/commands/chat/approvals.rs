use std::path::Path;

use anyhow::Result;
use gents::plugin::approval::{self, Answer, Request};

pub(super) fn mine(request: &Request, session_id: &str) -> bool {
    request
        .session_id
        .as_deref()
        .is_none_or(|id| id == session_id)
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

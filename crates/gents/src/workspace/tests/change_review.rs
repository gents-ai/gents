use super::*;
use crate::workspace::change_review::{inspect_immutable_changes, ReviewReference};

#[test]
fn immutable_review_preserves_later_files_index_and_private_untracked_content() {
    let fx = Fixture::new();
    fs::write(fx.repo.join("deleted.rs"), "original\n").unwrap();
    git(&fx.repo, &["add", "deleted.rs"]);
    git(&fx.repo, &["commit", "-m", "before"]);
    let before = git(&fx.repo, &["rev-parse", "HEAD"]);
    fs::write(fx.repo.join("README.md"), "recorded change\n").unwrap();
    fs::write(fx.repo.join("new.rs"), "created\n").unwrap();
    fs::remove_file(fx.repo.join("deleted.rs")).unwrap();
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["update-index", "--chmod=+x", "README.md"]);
    git(&fx.repo, &["commit", "-m", "recorded changes"]);
    let after = git(&fx.repo, &["rev-parse", "HEAD"]);

    fs::write(fx.repo.join("README.md"), "later commit\n").unwrap();
    git(&fx.repo, &["add", "README.md"]);
    git(&fx.repo, &["commit", "-m", "later user changes"]);
    fs::write(fx.repo.join("README.md"), "user staged\n").unwrap();
    git(&fx.repo, &["add", "README.md"]);
    fs::write(fx.repo.join("README.md"), "user working copy\n").unwrap();
    fs::write(fx.repo.join("private.local"), "private untracked content\n").unwrap();
    let index = fs::read(fx.repo.join(".git/index")).unwrap();
    let head = git(&fx.repo, &["rev-parse", "HEAD"]);

    let reference = ReviewReference {
        workspace_id: "workspace".into(),
        owner: "node".into(),
        base: before.clone(),
        capability: WorkspacePathCapability::exact_paths(vec![
            "README.md".into(),
            "deleted.rs".into(),
            "new.rs".into(),
        ])
        .unwrap(),
        tree: after.clone(),
        request_doc_id: "request-document".into(),
        review_id: "review".into(),
    };
    let review = inspect_immutable_changes(&fx.repo, &before, reference.clone()).unwrap();
    assert_eq!(review.edits.len(), 3);
    let readme = review
        .edits
        .iter()
        .find(|edit| edit.path == "README.md")
        .unwrap();
    assert_eq!(
        readme.before.as_ref().unwrap().blob,
        git(&fx.repo, &["rev-parse", &format!("{before}:README.md")])
    );
    assert_eq!(
        readme.after.as_ref().unwrap().blob,
        git(&fx.repo, &["rev-parse", &format!("{after}:README.md")])
    );
    assert!(!readme.before.as_ref().unwrap().executable);
    assert!(readme.after.as_ref().unwrap().executable);
    assert!(review
        .edits
        .iter()
        .find(|edit| edit.path == "new.rs")
        .unwrap()
        .before
        .is_none());
    assert!(review
        .edits
        .iter()
        .find(|edit| edit.path == "deleted.rs")
        .unwrap()
        .after
        .is_none());
    assert_eq!(fs::read(fx.repo.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(fx.repo.join("README.md")).unwrap(),
        "user working copy\n"
    );
    assert_eq!(
        fs::read_to_string(fx.repo.join("private.local")).unwrap(),
        "private untracked content\n"
    );
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), head);

    let mut empty = reference.clone();
    empty.tree.clone_from(&before);
    assert!(inspect_immutable_changes(&fx.repo, &before, empty)
        .unwrap()
        .edits
        .is_empty());
    assert!(inspect_immutable_changes(&fx.repo, "HEAD", reference.clone()).is_err());
    let mut unowned = reference.clone();
    unowned.capability = WorkspacePathCapability::exact_paths(Vec::new()).unwrap();
    assert!(inspect_immutable_changes(&fx.repo, &before, unowned).is_err());
    let mut missing = reference;
    missing.base = "f".repeat(40);
    assert!(inspect_immutable_changes(&fx.repo, &before, missing).is_err());
}

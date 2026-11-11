use gents_shell::source::{Change, ChangeKind};

/// Each file in `git diff` output, as the shell shows a change. An add's or a delete's diff is
/// its content without the sign, as a session records one; a file with no hunk, as a binary
/// file or a mode change, has none.
pub(super) fn git_changes(diff: &str) -> Vec<Change> {
    let mut files: Vec<Vec<&str>> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files.push(vec![line]);
        } else if let Some(file) = files.last_mut() {
            file.push(line);
        }
    }
    files.iter().map(|file| change(file)).collect()
}

/// `lines` start with the file's `diff --git` header.
fn change(lines: &[&str]) -> Change {
    let hunks = lines.iter().position(|line| line.starts_with("@@"));
    let (head, body) = lines.split_at(hunks.unwrap_or(lines.len()));
    let has = |prefix: &str| head.iter().any(|line| line.starts_with(prefix));
    let field = |prefix: &str| head.iter().find_map(|line| line.strip_prefix(prefix));
    // Git ends a `---` or `+++` name that holds a space with a tab, after any quote.
    let named = |prefix: &str, side: &str| -> Option<String> {
        let name = unquoted(field(prefix)?.trim_end_matches('\t'));
        name.strip_prefix(side).map(str::to_string)
    };
    let (kind, path) = if has("new file mode") {
        (ChangeKind::Add, named("+++ ", "b/"))
    } else if has("deleted file mode") {
        (ChangeKind::Delete, named("--- ", "a/"))
    } else if let (Some(from), Some(to)) = (field("rename from "), field("rename to ")) {
        let to = unquoted(to);
        (ChangeKind::Move { to }, Some(unquoted(from)))
    } else {
        (ChangeKind::Update, named("+++ ", "b/"))
    };
    let content = |sign: char| -> String {
        let lines = body.iter().filter_map(|line| line.strip_prefix(sign));
        lines.map(|line| format!("{line}\n")).collect()
    };
    let diff = match kind {
        ChangeKind::Add => content('+'),
        ChangeKind::Delete => content('-'),
        ChangeKind::Update | ChangeKind::Move { .. } => {
            body.iter().map(|line| format!("{line}\n")).collect()
        }
    };
    let path = path.or_else(|| header_path(head[0])).unwrap_or_default();
    Change::new(String::new(), path, kind, diff)
}

/// The `b/` name that ends a `diff --git` header, quoted or not.
fn header_path(header: &str) -> Option<String> {
    let at = if header.ends_with('"') {
        header.rfind(" \"b/")?
    } else {
        header.rfind(" b/")?
    };
    unquoted(&header[at + 1..])
        .strip_prefix("b/")
        .map(str::to_string)
}

/// Git C-quotes a name that holds a control character, a quote, a backslash or a byte past
/// ASCII: it escapes those as C does, and writes a byte past ASCII as three octal digits.
fn unquoted(name: &str) -> String {
    let Some(quoted) = name
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
    else {
        return name.to_string();
    };
    let quoted = quoted.as_bytes();
    let mut bytes = Vec::with_capacity(quoted.len());
    let mut at = 0;
    while at < quoted.len() {
        let (byte, read) = match (quoted[at], quoted.get(at + 1)) {
            (b'\\', Some(b'0'..=b'3')) => {
                let digits = quoted.get(at + 1..at + 4);
                let digits = digits.and_then(|digits| std::str::from_utf8(digits).ok());
                match digits.and_then(|digits| u8::from_str_radix(digits, 8).ok()) {
                    Some(byte) => (byte, 4),
                    None => (b'\\', 1),
                }
            }
            (b'\\', Some(&escaped)) => {
                let byte = match escaped {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    other => other,
                };
                (byte, 2)
            }
            (byte, _) => (byte, 1),
        };
        bytes.push(byte);
        at += read;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use gents_shell::source::ChangeKind;

    use super::git_changes;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 keep
-old
+new
+more
diff --git a/notes.txt b/notes.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/notes.txt
@@ -0,0 +1,3 @@
+one
+two
+three
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
diff --git a/old.rs b/new.rs
similarity index 80%
rename from old.rs
rename to new.rs
index 5555555..6666666 100644
--- a/old.rs
+++ b/new.rs
@@ -1 +1 @@
-a
+b
diff --git a/logo.png b/logo.png
index 7777777..8888888 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/blob.bin b/blob.bin
new file mode 100644
index 0000000000000000000000000000000000000000..8352675d67aed6625ece79af41c27fdb4ee2e867
GIT binary patch
literal 3
KcmZQzWC8#H2LJ>B

literal 0
HcmV?d00001

diff --git a/keep.rs b/kept.rs
similarity index 100%
rename from keep.rs
rename to kept.rs
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
";

    /// As git writes a name outside printable ASCII, and a quoted name holding a space.
    const QUOTED: &str = "\
diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"
index 7898192..6178079 100644
--- \"a/caf\\303\\251.rs\"
+++ \"b/caf\\303\\251.rs\"
@@ -1 +1 @@
-a
+b
diff --git \"a/new caf\\303\\251.rs\" \"b/new caf\\303\\251.rs\"
new file mode 100644
index 0000000..587be6b
--- /dev/null
+++ \"b/new caf\\303\\251.rs\"\t
@@ -0,0 +1 @@
+x
diff --git \"a/caf\\303\\251.rs\" \"b/tab\\there \\\"q\\\" \\\\.rs\"
similarity index 100%
rename from \"caf\\303\\251.rs\"
rename to \"tab\\there \\\"q\\\" \\\\.rs\"
";

    #[test]
    fn a_git_diff_becomes_one_change_per_file() {
        let changes = git_changes(DIFF);
        let seen: Vec<_> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.clone(), change.counts()))
            .collect();
        let moved = |to: &str| ChangeKind::Move { to: to.to_string() };
        assert_eq!(
            seen,
            [
                ("src/a.rs", ChangeKind::Update, (2, 1)),
                ("notes.txt", ChangeKind::Add, (3, 0)),
                ("gone.txt", ChangeKind::Delete, (0, 2)),
                ("old.rs", moved("new.rs"), (1, 1)),
                ("logo.png", ChangeKind::Update, (0, 0)),
                ("blob.bin", ChangeKind::Add, (0, 0)),
                ("keep.rs", moved("kept.rs"), (0, 0)),
                ("run.sh", ChangeKind::Update, (0, 0)),
            ]
        );
        assert_eq!(
            changes[0].diff,
            "@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+more\n"
        );
        assert_eq!(changes[1].diff, "one\ntwo\nthree\n");
        assert_eq!(changes[2].diff, "x\ny\n");
        assert_eq!(changes[3].diff, "@@ -1 +1 @@\n-a\n+b\n");
        for (file, why) in [
            (4, "a binary file has no hunks"),
            (5, "nor has a new binary file's patch"),
            (6, "nor a rename that kept the content"),
            (7, "nor a mode change"),
        ] {
            assert_eq!(changes[file].diff, "", "{why}");
        }
        assert!(changes.iter().all(|change| change.turn.is_empty()));
        assert!(git_changes("").is_empty());
    }

    #[test]
    fn quoted_names_are_unquoted() {
        let changes = git_changes(QUOTED);
        let seen: Vec<_> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind.clone(), change.counts()))
            .collect();
        let moved = ChangeKind::Move {
            to: "tab\there \"q\" \\.rs".to_string(),
        };
        assert_eq!(
            seen,
            [
                ("café.rs", ChangeKind::Update, (1, 1)),
                ("new café.rs", ChangeKind::Add, (1, 0)),
                ("café.rs", moved, (0, 0)),
            ]
        );
    }
}

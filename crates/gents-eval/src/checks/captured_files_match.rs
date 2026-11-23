use gents_documents::{EvalCapture, EvalStage};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::checks::captured_fields_match::{some, Test};
use crate::checks::{
    excerpt, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
    EXCERPT_CHARS,
};
use crate::runner::executor::{
    CaptureResult, FileRef, FileText, StageEvidence, FILE_TEXT_LIMIT_BYTES,
};

/// Grades the files a capture holds when the stage ends, never how they were written. Feedback states the expected content; it is written only on the train split, so the held-out split catches a candidate that hard-codes it.
pub struct CapturedFilesMatch;

/// External premise: `glob::glob`, which file captures walk with, matches each pattern component against one path component, so `*` never spans `/`, and a leading `.` needs no literal match.
const GLOB_WALK: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

const OUTSIDE: &str = "a link that resolves outside the trial workspace";

const NON_FILE: &str = "an entry that is not a regular file (a directory or a dangling link)";

const WHOLE_FILE_POINTER: &str =
    "pointer selects a JSON value, but exists and sha256 test the whole file; drop pointer";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    name: String,
    expect: Vec<Expectation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    path: String,
    #[serde(default)]
    pointer: Option<String>,
    #[serde(default)]
    exists: Option<bool>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default, deserialize_with = "some")]
    equals: Option<Value>,
    #[serde(default)]
    contains: Option<String>,
    #[serde(default)]
    matches: Option<String>,
}

struct Plan {
    name: String,
    expect: Vec<FileExpectation>,
}

struct FileExpectation {
    path: String,
    assertion: Assertion,
}

enum Assertion {
    Exists(bool),
    Sha256(String),
    TextEquals { text: String, sha256: String },
    Text(Test),
    Json { pointer: String, test: Test },
}

enum Found<'a> {
    File(&'a FileRef),
    Absent,
    Occupied(&'static str),
}

enum Judgement {
    Held,
    Unmet(String),
    Unavailable(String),
}

/// The capture a check ref reads and the paths whose text its expectations test, so the runner keeps text for exactly those.
pub fn text_paths(params: &Value) -> Option<(String, Vec<String>)> {
    let plan = plan(params).ok()?;
    let paths = plan
        .expect
        .into_iter()
        .filter(|expectation| expectation.assertion.reads_text())
        .map(|expectation| expectation.path)
        .collect();
    Some((plan.name, paths))
}

fn plan(params: &Value) -> Result<Plan, Vec<String>> {
    let params: Params =
        serde_json::from_value(params.clone()).map_err(|error| vec![error.to_string()])?;
    if params.expect.is_empty() {
        return Err(vec!["expect names no file".into()]);
    }
    let mut expect = Vec::new();
    let mut problems = Vec::new();
    for (index, expectation) in params.expect.into_iter().enumerate() {
        let path = expectation.path.clone();
        match expectation.assertion() {
            Ok(assertion) => expect.push(FileExpectation { path, assertion }),
            Err(problem) => problems.push(format!("expect[{index}] {path:?}: {problem}")),
        }
    }
    if problems.is_empty() {
        Ok(Plan {
            name: params.name,
            expect,
        })
    } else {
        Err(problems)
    }
}

impl Expectation {
    fn assertion(self) -> Result<Assertion, String> {
        let Self {
            path,
            pointer,
            exists,
            sha256,
            equals,
            contains,
            matches,
        } = self;
        if !path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        {
            return Err(
                "path must be relative to the trial workspace, with no empty, . or .. component"
                    .into(),
            );
        }
        let whole_file = |assertion| match pointer {
            None => Ok(assertion),
            Some(_) => Err(WHOLE_FILE_POINTER.to_owned()),
        };
        let test = match (exists, sha256, equals, contains, matches) {
            (Some(exists), None, None, None, None) => return whole_file(Assertion::Exists(exists)),
            (None, Some(sha256), None, None, None) => {
                if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(format!("sha256 {sha256:?} is not 64 hex digits"));
                }
                return whole_file(Assertion::Sha256(sha256.to_ascii_lowercase()));
            }
            (None, None, Some(expected), None, None) => Test::Equals(expected),
            (None, None, None, Some(needle), None) => Test::Contains(needle),
            (None, None, None, None, Some(pattern)) => Test::Matches(
                Regex::new(&pattern).map_err(|error| format!("matches is not a regex: {error}"))?,
            ),
            _ => {
                return Err(
                    "give exactly one of exists, sha256, equals, contains or matches".into(),
                )
            }
        };
        match (pointer, test) {
            (Some(pointer), test) => {
                if !(pointer.is_empty() || pointer.starts_with('/'))
                    || !pointer
                        .split('~')
                        .skip(1)
                        .all(|escaped| escaped.starts_with(['0', '1']))
                {
                    return Err(format!(
                        "pointer {pointer:?} is not a JSON pointer: empty, or each token after a /, with ~ written ~0 and / written ~1"
                    ));
                }
                Ok(Assertion::Json { pointer, test })
            }
            (None, Test::Equals(Value::String(text))) => Ok(Assertion::TextEquals {
                sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
                text,
            }),
            (None, Test::Equals(other)) => Err(format!(
                "equals without pointer is the file's whole text, so it must be a string, not {other}; add pointer to compare a JSON value"
            )),
            (None, test) => Ok(Assertion::Text(test)),
        }
    }
}

impl Assertion {
    fn reads_text(&self) -> bool {
        matches!(
            self,
            Self::TextEquals { .. } | Self::Text(_) | Self::Json { .. }
        )
    }

    fn describe(&self) -> String {
        match self {
            Self::Exists(true) => "present".into(),
            Self::Exists(false) => "absent".into(),
            Self::Sha256(sha256) => format!("sha256 {sha256}"),
            Self::TextEquals { text, .. } => format!("text equal to {text:?}"),
            Self::Text(test) => test.describe(),
            Self::Json { pointer, test } => format!("JSON at {pointer:?} {}", test.describe()),
        }
    }

    fn judge(&self, found: Found<'_>) -> Judgement {
        let file = match (self, found) {
            (Self::Exists(false), Found::Absent) => return Judgement::Held,
            (Self::Exists(false), Found::File(file)) => {
                return Judgement::Unmet(format!("a file of {} bytes", file.bytes))
            }
            (_, Found::Absent) => return Judgement::Unmet("absent".into()),
            (_, Found::Occupied(what)) => return Judgement::Unmet(what.into()),
            (_, Found::File(file)) => file,
        };
        match self {
            Self::Exists(_) => Judgement::Held,
            Self::Sha256(sha256) | Self::TextEquals { sha256, .. } if *sha256 == file.sha256 => {
                Judgement::Held
            }
            Self::TextEquals { text, .. } => Judgement::Unmet(match &file.text {
                Some(FileText::Kept(actual)) => first_difference(text, actual),
                _ => digest_of(file),
            }),
            Self::Sha256(_) => Judgement::Unmet(digest_of(file)),
            Self::Text(test) | Self::Json { test, .. } => {
                let text = match &file.text {
                    Some(FileText::Kept(text)) => text,
                    Some(FileText::NotUtf8) => {
                        return Judgement::Unmet("bytes that are not UTF-8 text".into())
                    }
                    Some(FileText::OverLimit) => {
                        return Judgement::Unavailable(format!(
                            "it holds {} bytes and a file capture keeps text only up to {FILE_TEXT_LIMIT_BYTES}",
                            file.bytes
                        ))
                    }
                    None => {
                        return Judgement::Unavailable(
                            "the capture kept no text for it; text is kept only for paths this stage's captured_files_match expectations name".into(),
                        )
                    }
                };
                let actual = match self {
                    Self::Json { pointer, .. } => match serde_json::from_str::<Value>(text) {
                        Err(error) => {
                            return Judgement::Unmet(format!("text that is not JSON ({error})"))
                        }
                        Ok(document) => match document.pointer(pointer) {
                            None => return Judgement::Unmet("no value there".into()),
                            Some(value) => value.clone(),
                        },
                    },
                    _ => Value::String(text.clone()),
                };
                if test.holds(&actual) {
                    Judgement::Held
                } else {
                    Judgement::Unmet(excerpt(&actual.to_string(), EXCERPT_CHARS))
                }
            }
        }
    }
}

fn digest_of(file: &FileRef) -> String {
    format!("sha256 {} ({} bytes)", file.sha256, file.bytes)
}

fn first_difference(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.split('\n').collect();
    let actual: Vec<&str> = actual.split('\n').collect();
    let line = (0..expected.len().max(actual.len()))
        .find(|line| expected.get(*line) != actual.get(*line))
        .unwrap_or_default();
    let shown = |text: Option<&&str>| {
        text.map_or_else(
            || "the end of the file".to_owned(),
            |text| format!("{:?}", excerpt(text, EXCERPT_CHARS)),
        )
    };
    format!(
        "line {} {} where {} was expected",
        line + 1,
        shown(actual.get(line)),
        shown(expected.get(line))
    )
}

fn find<'a>(
    path: &str,
    files: &'a [FileRef],
    outside: &[String],
    non_files: &[String],
) -> Found<'a> {
    let names = |paths: &[String]| paths.iter().any(|named| named == path);
    match files.iter().find(|file| file.path == path) {
        Some(file) => Found::File(file),
        None if names(outside) => Found::Occupied(OUTSIDE),
        None if names(non_files) => Found::Occupied(NON_FILE),
        None => Found::Absent,
    }
}

impl Check for CapturedFilesMatch {
    fn name(&self) -> &'static str {
        "captured_files_match"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn describe(&self) -> CheckDescription {
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: format!("Scores the fraction of expectations that hold over a file capture, each naming one workspace-relative path and exactly one test of that file as the stage left it: exists (true present, false absent), sha256 of its bytes, equals (its whole text, or with pointer the JSON value there), contains or matches (its text, or with pointer the value there). A test of an absent file fails, and so does exists false where a directory or link sits. The capture keeps text only for the paths these expectations name, up to {FILE_TEXT_LIMIT_BYTES} bytes each; sha256 and whole-text equals grade a file of any size."),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "a file capture of the same stage"},
                    "expect": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string", "minLength": 1, "description": "the file's path relative to the trial workspace, as the capture's glob reaches it; not a glob"},
                                "pointer": {"type": "string", "pattern": "^(/([^~]|~[01])*)*$", "description": "RFC 6901 JSON pointer (\"\" is the whole document); the file is parsed as JSON and equals, contains or matches tests the value there"},
                                "exists": {"type": "boolean", "description": "true: the capture holds the file; false: it does not"},
                                "sha256": {"type": "string", "pattern": "^[0-9a-fA-F]{64}$", "description": "hex digest of the file's exact bytes"},
                                "equals": {"description": "without pointer, a string equal to the file's whole text; with pointer, the JSON value there"},
                                "contains": {"type": "string"},
                                "matches": {"type": "string", "description": "a regex"}
                            },
                            "required": ["path"],
                            "oneOf": [
                                {"required": ["exists"]},
                                {"required": ["sha256"]},
                                {"required": ["equals"]},
                                {"required": ["contains"]},
                                {"required": ["matches"]}
                            ],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["name", "expect"],
                "additionalProperties": false
            }),
            reads: vec!["capture:files".into()],
            reason_codes: graded_reason_codes(&[
                ("missing_capture", "grader: no file capture of that name"),
                (
                    "content_unavailable",
                    "grader: contains, matches or pointer tests a file whose text the capture did not keep (too large, or a path no expectation named when it ran)",
                ),
            ]),
        }
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let plan = match plan(params) {
            Ok(plan) => plan,
            Err(problems) => return grader("bad_params", problems.join("; ")),
        };
        let name = &plan.name;
        let (files, outside, non_files) = match stage.captures.get(name) {
            Some(CaptureResult::Files {
                files,
                outside_workspace,
                non_files,
            }) => (files, outside_workspace, non_files),
            Some(CaptureResult::Documents { .. } | CaptureResult::Schema { .. }) => {
                return grader(
                    "missing_capture",
                    format!("capture {name} does not hold files"),
                )
            }
            None => {
                return grader(
                    "missing_capture",
                    format!("the stage produced no capture named {name}"),
                )
            }
        };
        let mut satisfied = 0;
        let mut lines = Vec::new();
        for FileExpectation { path, assertion } in &plan.expect {
            match assertion.judge(find(path, files, outside, non_files)) {
                Judgement::Held => satisfied += 1,
                Judgement::Unmet(got) => lines.push(format!(
                    "{path}: expected {}, got {got}",
                    excerpt(&assertion.describe(), EXCERPT_CHARS)
                )),
                Judgement::Unavailable(why) => {
                    return grader(
                        "content_unavailable",
                        format!("{path}: {why}, so contains, matches and pointer cannot test it; sha256 and whole-text equals can"),
                    )
                }
            }
        }
        let feedback = (!lines.is_empty()).then(|| lines.join("\n"));
        graded(satisfied, plan.expect.len(), feedback)
    }

    fn conformance(&self, params: &Value, stage: &EvalStage) -> Vec<String> {
        let plan = match plan(params) {
            Ok(plan) => plan,
            Err(problems) => return problems,
        };
        let name = &plan.name;
        let glob = match stage.capture.iter().find(|capture| capture.name() == name) {
            Some(EvalCapture::File { glob, .. }) => glob,
            Some(_) => {
                return vec![format!(
                    "capture {name:?} does not capture files; name a file capture of this stage"
                )]
            }
            None => {
                return vec![format!(
                    "the stage declares no capture {name:?}; add {{\"kind\": \"file\", \"name\": {name:?}, \"glob\": ...}} to its capture"
                )]
            }
        };
        if glob.rsplit('/').next() == Some("**") {
            return vec![format!(
                "capture {name}'s glob {glob:?} ends in **, which reaches directories only, so the capture holds no file; end it in **/* instead"
            )];
        }
        let pattern = match glob::Pattern::new(glob) {
            Ok(pattern) => pattern,
            Err(error) => {
                return vec![format!(
                    "capture {name}'s glob {glob:?} does not compile: {error}"
                )]
            }
        };
        plan.expect
            .iter()
            .enumerate()
            .filter(|(_, expectation)| !pattern.matches_with(&expectation.path, GLOB_WALK))
            .map(|(index, expectation)| {
                format!(
                    "expect[{index}] {:?} is outside capture {name}'s glob {glob:?}, so the capture never holds it",
                    expectation.path
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;

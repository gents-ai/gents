//! Runs every plugin case file natively and compares it with its recorded
//! output. The same files run through the WebAssembly host under
//! `gents pack test`, so a difference between the two is a bug in one of them.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn case_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|f| f.extension().is_some_and(|e| e == "json"));
    files.sort();
    files
}

fn run_case(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let case: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut input = case["input"].clone();
    if let Some(bind) = case["bind"].as_str() {
        let dir = path.parent().unwrap_or(Path::new(".")).join(bind);
        let dir = dir.canonicalize().map_err(|e| format!("{name}: {e}"))?;
        input["path"] = Value::String(dir.to_string_lossy().into_owned());
    }
    let want = case
        .get("expect")
        .ok_or_else(|| format!("{name} has no recorded output"))?;
    let got: Value =
        serde_json::from_str(&crate::run(&input.to_string()).map_err(|e| format!("{name}: {e}"))?)
            .map_err(|e| e.to_string())?;
    if &got != want {
        return Err(format!("{name}: the output differs from the recorded one"));
    }
    Ok(())
}

fn check_dir(dir: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let files = case_files(&dir);
    assert!(
        files.len() >= 5,
        "{} has {} cases",
        dir.display(),
        files.len()
    );
    let failures: Vec<String> = files.iter().filter_map(|f| run_case(f).err()).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_charts_case_reproduces_its_recorded_output() {
    check_dir("tests");
}

#[test]
fn every_case_covers_what_its_name_says() {
    // The names are the documentation: one chart type or behaviour each, and a recorded output.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for f in case_files(&dir) {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let case: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert!(case.get("input").is_some_and(Value::is_object), "{name}");
        assert!(
            case.get("expect").is_some_and(|e| !e.is_null()),
            "{name} needs a recorded output"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.'),
            "{name}"
        );
    }
}

#[test]
fn every_chart_type_and_input_format_has_a_case() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut charts = std::collections::BTreeSet::new();
    let mut extensions = std::collections::BTreeSet::new();
    let (mut bound_file, mut bound_folder, mut inline, mut graph) = (false, false, false, false);
    for f in case_files(&dir) {
        let case: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        let input = &case["input"];
        if let Some(c) = input["chart"].as_str() {
            charts.insert(c.to_owned());
        }
        graph |= input.get("run_id").is_some();
        inline |= input.get("data").is_some() && case.get("bind").is_none();
        if let Some(file) = input["file"].as_str() {
            bound_file |= file.contains('.') || !file.contains('/');
            bound_folder |= file.contains('/') || case["bind"] == "fixtures";
            if let Some(ext) = Path::new(file).extension() {
                extensions.insert(ext.to_string_lossy().into_owned());
            }
        }
    }
    for chart in [
        "line",
        "area",
        "stacked_area",
        "bar",
        "stacked_bar",
        "horizontal_bar",
        "scatter",
        "bubble",
        "histogram",
        "box",
        "pie",
        "donut",
        "heatmap",
        "combo",
    ] {
        assert!(charts.contains(chart), "no case draws a {chart} chart");
    }
    for ext in ["csv", "json", "ndjson", "tsv", "png"] {
        assert!(extensions.contains(ext), "no case reads a .{ext} file");
    }
    assert!(bound_file && bound_folder && inline && graph);
}

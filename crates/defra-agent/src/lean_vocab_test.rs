pub(crate) fn lean_to_defradb_values(model: &'static str, namespace: &str) -> Vec<&'static str> {
    let mut in_namespace = false;
    let mut in_to_defradb = false;
    let mut values = Vec::new();

    for line in model.lines() {
        let trimmed = line.trim();
        if trimmed == format!("namespace {namespace}") {
            in_namespace = true;
            continue;
        }
        if in_namespace && trimmed == format!("end {namespace}") {
            break;
        }
        if in_namespace && trimmed.starts_with("def toDefraDB") {
            in_to_defradb = true;
            continue;
        }
        if !in_to_defradb {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("| .") else {
            if !values.is_empty() && !trimmed.is_empty() {
                break;
            }
            continue;
        };
        let (_constructor, value) = rest
            .split_once("=>")
            .expect("Lean toDefraDB arm must contain =>");
        values.push(
            value
                .trim()
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .expect("Lean toDefraDB arm must return a string literal"),
        );
    }

    assert!(
        !values.is_empty(),
        "missing Lean toDefraDB values for namespace {namespace}"
    );
    values
}

use super::parser::{segment_markdown, MarkdownSegment};
use super::CellRun;

#[test]
fn pure_prose_returns_single_prose_segment() {
    let segments = segment_markdown("hello `world` and **bold**");
    assert_eq!(segments.len(), 1);
    assert!(matches!(segments[0], MarkdownSegment::Prose(_)));
}

#[test]
fn prose_with_six_fenced_code_blocks_renders_as_single_prose() {
    let body = r#"
## Repo Overview

**defra-agent** is a Rust agent runtime.

```rust
fn main() {}
```

```toml
name = "x"
```

```bash
echo hi
```

```json
{"k":1}
```

```lean
theorem t : True := trivial
```

```txt
plain
```

tail prose here.
"#;
    let segments = segment_markdown(body);
    assert_eq!(
        segments.len(),
        1,
        "no tables, so the whole body should be one prose segment"
    );
    assert!(matches!(segments[0], MarkdownSegment::Prose(_)));
    let MarkdownSegment::Prose(ref prose) = segments[0] else {
        unreachable!()
    };
    assert!(prose.contains("tail prose here"));
}

#[test]
fn keyfiles_response_segments_keep_prose_after_table() {
    let text = include_str!("../../fixtures/keyfiles_response.md");
    let segments = segment_markdown(text);
    let kinds: Vec<&str> = segments
        .iter()
        .map(|segment| match segment {
            MarkdownSegment::Prose(_) => "prose",
            MarkdownSegment::Table(_) => "table",
        })
        .collect();
    assert!(
        kinds.iter().any(|k| *k == "table"),
        "expected at least one table segment, got {:?}",
        kinds
    );
    assert!(
        kinds.last().map(|k| *k == "prose").unwrap_or(false),
        "expected trailing prose segment after the table, got {:?}",
        kinds
    );
    let MarkdownSegment::Prose(tail) = segments.last().unwrap() else {
        panic!("last segment must be prose");
    };
    assert!(
        tail.contains("Important Constraints"),
        "trailing prose should contain the post-table section; got:\n{}",
        tail
    );
}

#[test]
fn pipe_table_is_parsed_into_headers_and_rows() {
    let text = "intro\n\n| Crate | Purpose |\n| --- | --- |\n| `defra-agent` | Runtime library |\n| `defra-agent-cli` | Compiled CLI (`defra-agent`) |\n\nafter";
    let segments = segment_markdown(text);
    let kinds: Vec<&str> = segments
        .iter()
        .map(|segment| match segment {
            MarkdownSegment::Prose(_) => "prose",
            MarkdownSegment::Table(_) => "table",
        })
        .collect();
    assert_eq!(kinds, ["prose", "table", "prose"]);

    let MarkdownSegment::Table(table) = &segments[1] else {
        panic!("middle segment must be a table");
    };
    let flatten = |cell: &[CellRun]| -> String {
        cell.iter().map(|run| run.text.as_str()).collect::<String>()
    };
    assert_eq!(flatten(&table.headers[0]), "Crate");
    assert_eq!(flatten(&table.headers[1]), "Purpose");
    assert_eq!(table.rows.len(), 2);
    assert_eq!(flatten(&table.rows[0][0]), "defra-agent");
    assert_eq!(flatten(&table.rows[0][1]), "Runtime library");
    assert_eq!(flatten(&table.rows[1][0]), "defra-agent-cli");
    assert_eq!(flatten(&table.rows[1][1]), "Compiled CLI (defra-agent)");
    assert!(
        table.rows[0][0][0].style.code,
        "first cell's inline code run must be flagged as code"
    );
}

#[test]
fn table_cells_preserve_strong_emphasis_and_link_styling() {
    let text = concat!(
        "| Case | Cell |\n",
        "| --- | --- |\n",
        "| bold | **load-bearing** |\n",
        "| italic | *load-bearing* |\n",
        "| link | [load-bearing](https://example.invalid/docs) |\n",
    );
    let segments = segment_markdown(text);
    let MarkdownSegment::Table(table) = segments
        .iter()
        .find(|s| matches!(s, MarkdownSegment::Table(_)))
        .expect("a table segment")
    else {
        unreachable!();
    };
    let bold = &table.rows[0][1];
    assert_eq!(bold.len(), 1);
    assert!(bold[0].style.strong, "strong run must be flagged");
    assert_eq!(bold[0].text, "load-bearing");

    let italic = &table.rows[1][1];
    assert_eq!(italic.len(), 1);
    assert!(italic[0].style.emphasis, "emphasis run must be flagged");

    let link = &table.rows[2][1];
    assert_eq!(link.len(), 1);
    assert_eq!(
        link[0].style.link.as_deref(),
        Some("https://example.invalid/docs")
    );
    assert_eq!(link[0].text, "load-bearing");
}

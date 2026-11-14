use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

struct FieldDef {
    name: String,
    kind: &'static str,
}

struct TypeDef {
    name: String,
    fields: Vec<FieldDef>,
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let mut types = BTreeMap::new();
    let schema_dirs = [
        manifest.join("schemas/agent"),
        manifest.join("../gents-protocol/schemas/inference"),
        manifest.join("../gents-protocol/schemas/services"),
    ];
    for schema_dir in &schema_dirs {
        track_schema(schema_dir);
    }
    let mut paths: Vec<_> = schema_dirs
        .iter()
        .flat_map(|schema_dir| {
            fs::read_dir(schema_dir)
                .expect("schema directory")
                .map(|entry| entry.expect("schema entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "graphql"))
        })
        .collect();
    paths.sort();
    for path in paths {
        track_schema(&path);
        for parsed in parse_types(&path) {
            let name = parsed.name.clone();
            assert!(
                types.insert(name.clone(), parsed).is_none(),
                "duplicate schema type {name}"
            );
        }
    }

    let mut out = String::new();
    for def in types.values() {
        render_type(&mut out, def);
    }
    let dest = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("fields.rs");
    fs::write(dest, out).expect("write generated fields");
}

fn track_schema(path: &Path) {
    use std::io::Write;
    writeln!(
        std::io::stdout(),
        "cargo:rerun-if-changed={}",
        path.display()
    )
    .expect("write Cargo schema dependency");
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

/// The schema bundle uses one field per line and no nested selections, so the
/// only brace that opens a body is the first one outside a directive argument.
fn parse_types(path: &Path) -> Vec<TypeDef> {
    let source = fs::read_to_string(path).expect("read schema");
    let text: String = source
        .lines()
        .map(strip_comment)
        .collect::<Vec<_>>()
        .join("\n");
    let mut types = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = find_type_keyword(rest) {
        rest = &rest[start + "type".len()..];
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        assert!(!name.is_empty(), "unnamed type in {}", path.display());
        let open = body_open(rest).unwrap_or_else(|| panic!("{name} has no body"));
        let close = rest[open..]
            .find('}')
            .unwrap_or_else(|| panic!("{name} body is not closed"))
            + open;
        let fields = rest[open + 1..close]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| parse_field(&name, line))
            .collect();
        types.push(TypeDef { name, fields });
        rest = &rest[close + 1..];
    }
    assert!(!types.is_empty(), "no type in {}", path.display());
    types
}

fn find_type_keyword(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("type ") || trimmed.starts_with("type\t") {
            return Some(offset + (line.len() - trimmed.len()));
        }
        offset += line.len();
    }
    None
}

fn body_open(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            '{' if !in_string && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_field(type_name: &str, line: &str) -> FieldDef {
    let (name, rest) = line
        .split_once(':')
        .unwrap_or_else(|| panic!("{type_name}: unparsed schema line {line:?}"));
    let name = name.trim().to_owned();
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        "{type_name}: invalid field name in {line:?}"
    );
    let declared = rest
        .trim()
        .split(|ch: char| ch.is_whitespace() || ch == '@')
        .next()
        .unwrap_or_default();
    let kind = match declared.trim_end_matches('!') {
        "String" | "ID" => "Str",
        "Int" => "Int",
        "Float" => "Float",
        "Boolean" => "Bool",
        "DateTime" => "DateTime",
        "JSON" => "Json",
        "Blob" => "Blob",
        "[String]" | "[String!]" => "StrList",
        "[Int]" | "[Int!]" => "IntList",
        other => panic!("{type_name}.{name}: unsupported schema type {other:?}"),
    };
    FieldDef { name, kind }
}

fn module_name(type_name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = type_name.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let prev_lower = index > 0 && !chars[index - 1].is_ascii_uppercase();
            let next_lower = chars
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_lowercase());
            if index > 0 && (prev_lower || next_lower) {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(*ch);
        }
    }
    out
}

fn render_type(out: &mut String, def: &TypeDef) {
    const RESERVED: [&str; 3] = ["DOC_ID", "FIELD_NAMES", "SELECT_ALL"];
    let module = module_name(&def.name);
    writeln!(out, "pub mod {module} {{").unwrap();
    writeln!(out, "    use crate::{{kind, Collection, Field}};").unwrap();
    writeln!(out, "    #[derive(Debug, Clone, Copy, PartialEq, Eq)]").unwrap();
    writeln!(out, "    pub enum Doc {{}}").unwrap();
    writeln!(out, "    impl Collection for Doc {{").unwrap();
    writeln!(out, "        const NAME: &'static str = {:?};", def.name).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(
        out,
        "    pub const DOC_ID: Field<Doc, kind::DocId> = Field::new(\"_docID\");"
    )
    .unwrap();
    let mut names = Vec::new();
    for field in &def.fields {
        let constant = field.name.to_ascii_uppercase();
        assert!(
            !RESERVED.contains(&constant.as_str()),
            "{}.{} collides with a generated constant",
            def.name,
            field.name
        );
        writeln!(
            out,
            "    pub const {constant}: Field<Doc, kind::{}> = Field::new({:?});",
            field.kind, field.name
        )
        .unwrap();
        names.push(format!("{:?}", field.name));
    }
    writeln!(
        out,
        "    pub const FIELD_NAMES: &[&str] = &[{}];",
        names.join(", ")
    )
    .unwrap();
    writeln!(out, "    impl crate::SelectFields for Doc {{").unwrap();
    writeln!(
        out,
        "        fn select_fields<S>(selection: S) -> S where S: crate::FieldSelection<Self> {{"
    )
    .unwrap();
    write!(out, "            selection").unwrap();
    for field in &def.fields {
        write!(out, ".field({})", field.name.to_ascii_uppercase()).unwrap();
    }
    writeln!(out, "\n        }}\n    }}").unwrap();
    let is_str = |name: &str| {
        def.fields
            .iter()
            .any(|field| field.name == name && field.kind == "Str")
    };
    if is_str("node_did") && is_str("session_id") && is_str("requester_did") {
        writeln!(out, "    impl crate::SessionScoped for Doc {{").unwrap();
        writeln!(
            out,
            "        const NODE_DID: Field<Doc, kind::Str> = NODE_DID;"
        )
        .unwrap();
        writeln!(
            out,
            "        const SESSION_ID: Field<Doc, kind::Str> = SESSION_ID;"
        )
        .unwrap();
        writeln!(
            out,
            "        const REQUESTER_DID: Field<Doc, kind::Str> = REQUESTER_DID;"
        )
        .unwrap();
        writeln!(out, "    }}").unwrap();
    }
    writeln!(out, "}}").unwrap();
}

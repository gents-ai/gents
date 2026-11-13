use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

#[derive(Default)]
struct Module {
    structs: BTreeMap<String, (String, Vec<String>)>,
    bounds: BTreeMap<String, String>,
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "type", "unsafe", "use", "where", "while",
];

impl Module {
    fn rust_type(&mut self, schema: &Value, enums: &mut Vec<String>) -> String {
        if let Some(scalar) = schema.as_str() {
            return match scalar {
                "bool" => "bool".into(),
                "nat" => "u64".into(),
                "int" => "i64".into(),
                "string" => "String".into(),
                "json" => "crate::family::Json".into(),
                other => panic!("unknown family scalar {other}"),
            };
        }
        if let Some(item) = schema.get("option") {
            return format!("Option<{}>", self.rust_type(item, enums));
        }
        if let Some(item) = schema.get("list") {
            return format!("Vec<{}>", self.rust_type(item, enums));
        }
        if let Some(name) = schema.get("enum").and_then(Value::as_str) {
            if !enums.iter().any(|known| known == name) {
                enums.push(name.to_owned());
            }
            return name.to_owned();
        }
        let name = schema["struct"].as_str().expect("family struct name");
        let mut used = Vec::new();
        let mut body = String::new();
        let mut optional = false;
        for field in schema["fields"].as_array().expect("family struct fields") {
            let field_type = self.rust_type(&field[1], &mut used);
            let field_name = field[0].as_str().expect("family field name");
            if field[1].get("option").is_some() {
                optional = true;
                writeln!(
                    body,
                    "        #[serde(deserialize_with = \"crate::required_nullable\")]"
                )
                .unwrap();
            }
            let raw = if KEYWORDS.contains(&field_name) {
                "r#"
            } else {
                ""
            };
            writeln!(body, "        pub {raw}{field_name}: {field_type},").unwrap();
        }
        used.sort();
        if optional && !used.is_empty() {
            let bounds: Vec<String> = used
                .iter()
                .map(|parameter| format!("{parameter}: serde::Deserialize<'de>"))
                .collect();
            self.bounds.insert(name.to_owned(), bounds.join(", "));
        }
        let rendered = generic(name, &used);
        for parameter in &used {
            if !enums.contains(parameter) {
                enums.push(parameter.clone());
            }
        }
        let previous = self.structs.insert(name.to_owned(), (body.clone(), used));
        assert!(
            previous.is_none_or(|(known, _)| known == body),
            "family struct {name} has two shapes"
        );
        rendered
    }
}

fn generic(name: &str, parameters: &[String]) -> String {
    if parameters.is_empty() {
        name.to_owned()
    } else {
        format!("{name}<{}>", parameters.join(", "))
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let path = manifest.join("families.schema.json");
    println!("cargo:rerun-if-changed={}", path.display());
    let schema: BTreeMap<String, BTreeMap<String, Value>> =
        serde_json::from_str(&fs::read_to_string(&path).expect("family schema"))
            .expect("family schema JSON");

    let mut out = String::new();
    for key in schema["vocabularies"].keys() {
        assert!(
            !schema["functions"].contains_key(key),
            "family key {key} names a function and a vocabulary"
        );
        writeln!(
            out,
            "pub mod {key} {{\n    \
             pub fn replay<T>(all: &[T])\n    where\n        \
             T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,\n    {{\n        \
             crate::family::replay_vocabulary({key:?}, all)\n    }}\n}}"
        )
        .unwrap();
    }
    for (key, family) in &schema["functions"] {
        let mut module = Module::default();
        let mut enums = Vec::new();
        let input = module.rust_type(&family["input"], &mut enums);
        let output = module.rust_type(&family["output"], &mut enums);
        enums.sort();
        writeln!(out, "pub mod {key} {{").unwrap();
        for (name, (body, parameters)) in &module.structs {
            writeln!(
                out,
                "    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n    \
                 #[serde(deny_unknown_fields{})]\n    pub struct {} {{\n{body}    }}",
                module
                    .bounds
                    .get(name)
                    .map_or(String::new(), |bounds| format!(
                        ", bound(deserialize = \"{bounds}\")"
                    )),
                generic(name, parameters)
            )
            .unwrap();
        }
        writeln!(
            out,
            "    pub fn replay<{parameters}>(run: impl Fn(&{input}) -> {output})\n    where\n        \
             {input}: serde::de::DeserializeOwned,\n        {output}: serde::Serialize,\n    {{\n        \
             crate::family::replay({key:?}, run)\n    }}\n    \
             pub fn cases<{parameters}>() -> Vec<crate::family::Case<{input}, {output}>>\n    where\n        \
             {input}: serde::de::DeserializeOwned,\n    {{\n        \
             crate::family::cases({key:?})\n    }}\n}}",
            parameters = enums.join(", "),
        )
        .unwrap();
    }
    let dest = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("families.rs");
    fs::write(dest, out).expect("write generated families");
}

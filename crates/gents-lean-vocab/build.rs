use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

/// One family's Rust types. A Lean enum becomes a type parameter, so the
/// production enum is the decoder and its serde names are the checked vocabulary.
#[derive(Default)]
struct Module {
    structs: BTreeMap<String, (String, Vec<String>)>,
}

impl Module {
    fn rust_type(&mut self, schema: &Value, enums: &mut Vec<String>) -> String {
        if let Some(scalar) = schema.as_str() {
            return match scalar {
                "bool" => "bool".into(),
                "nat" => "u64".into(),
                "string" => "String".into(),
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
        for field in schema["fields"].as_array().expect("family struct fields") {
            let field_type = self.rust_type(&field[1], &mut used);
            let field_name = field[0].as_str().expect("family field name");
            writeln!(body, "        pub {field_name}: {field_type},").unwrap();
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
    for (key, vocabulary) in &schema["vocabularies"] {
        assert!(
            !schema["functions"].contains_key(key),
            "family key {key} names a function and a vocabulary"
        );
        let name = vocabulary["enum"].as_str().expect("vocabulary enum name");
        writeln!(
            out,
            "pub mod {key} {{\n    \
             /// Checks that `all`, every value of the production enum, is Lean `{name}`\n    \
             /// under the same wire names.\n    \
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
        writeln!(out, "pub mod {key} {{").unwrap();
        for (name, (body, parameters)) in &module.structs {
            writeln!(
                out,
                "    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\n    \
                 #[serde(deny_unknown_fields)]\n    pub struct {} {{\n{body}    }}",
                generic(name, parameters)
            )
            .unwrap();
        }
        writeln!(
            out,
            "    /// Replays every Lean case of `{key}` through the production owner.\n    \
             pub fn replay<{parameters}>(run: impl Fn(&{input}) -> {output})\n    where\n        \
             {input}: serde::de::DeserializeOwned,\n        {output}: serde::Serialize,\n    {{\n        \
             crate::family::replay({key:?}, run)\n    }}\n}}",
            parameters = enums.join(", "),
        )
        .unwrap();
    }
    let dest = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("families.rs");
    fs::write(dest, out).expect("write generated families");
}

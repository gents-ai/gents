use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use heck::ToLowerCamelCase;
use syn::{FnArg, GenericArgument, Item, Pat, PathArguments, ReturnType, Type, UseTree};

fn imports(tree: &UseTree, prefix: &str, names: &mut BTreeMap<String, String>) {
    match tree {
        UseTree::Path(path) => imports(&path.tree, &format!("{prefix}{}::", path.ident), names),
        UseTree::Name(name) => {
            names.insert(name.ident.to_string(), format!("{prefix}{}", name.ident));
        }
        UseTree::Rename(rename) => {
            names.insert(
                rename.rename.to_string(),
                format!("{prefix}{}", rename.ident),
            );
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                imports(tree, prefix, names);
            }
        }
        UseTree::Glob(_) => {}
    }
}

struct Types {
    files: BTreeMap<String, String>,
    imports: BTreeSet<String>,
    aliases: BTreeMap<String, String>,
}

impl Types {
    fn name(&mut self, ty: &Type) -> Result<String, String> {
        match ty {
            Type::Tuple(tuple) if tuple.elems.is_empty() => Ok("void".into()),
            Type::Path(path) if path.qself.is_none() => {
                let segment = path.path.segments.last().ok_or("empty type path")?;
                let mut name = segment.ident.to_string();
                if let Some(alias) = self.aliases.get(&name) {
                    name = alias.rsplit("::").next().unwrap().to_owned();
                }
                let arguments = match &segment.arguments {
                    PathArguments::None => Vec::new(),
                    PathArguments::AngleBracketed(args) => args
                        .args
                        .iter()
                        .map(|arg| match arg {
                            GenericArgument::Type(ty) => self.name(ty),
                            _ => Err("unsupported wire type argument".into()),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => return Err("unsupported wire type arguments".into()),
                };
                match (name.as_str(), arguments.as_slice()) {
                    ("Option", [value]) => Ok(format!("{value} | null")),
                    ("Vec", [value]) => Ok(format!("Array<{value}>")),
                    ("String" | "PathBuf", []) => Ok("string".into()),
                    ("bool", []) => Ok("boolean".into()),
                    (
                        "usize" | "isize" | "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32"
                        | "i64" | "f32" | "f64",
                        [],
                    ) => Ok("number".into()),
                    ("Value", []) => self.named("JsonValue"),
                    (_, []) => self.named(&name),
                    _ => Err(format!("unsupported wire type {name}")),
                }
            }
            _ => Err("unsupported wire type".into()),
        }
    }

    fn result(&mut self, ty: &Type) -> Result<String, String> {
        if let Type::Path(path) = ty {
            if let Some(segment) = path
                .path
                .segments
                .last()
                .filter(|part| part.ident == "Result")
            {
                if let PathArguments::AngleBracketed(arguments) = &segment.arguments {
                    if let Some(GenericArgument::Type(value)) = arguments.args.first() {
                        return self.name(value);
                    }
                }
                return Err("a command Result requires a success type".into());
            }
        }
        self.name(ty)
    }

    fn named(&mut self, name: &str) -> Result<String, String> {
        let file = self
            .files
            .get(name)
            .ok_or_else(|| format!("wire type {name} has no generated binding"))?;
        self.imports.insert(format!(
            "import type {{ {name} }} from \"./{}.js\";",
            file.trim_end_matches(".ts")
        ));
        Ok(name.into())
    }

    fn injected(&self, ty: &Type) -> bool {
        let Type::Path(path) = ty else { return false };
        let Some(first) = path.path.segments.first() else {
            return false;
        };
        let qualified = self
            .aliases
            .get(&first.ident.to_string())
            .cloned()
            .unwrap_or_else(|| {
                path.path
                    .segments
                    .iter()
                    .map(|part| part.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::")
            });
        matches!(
            qualified.as_str(),
            "tauri::State" | "tauri::AppHandle" | "tauri::Window" | "tauri::WebviewWindow"
        )
    }
}

pub(super) fn export(dir: &Path) -> Result<(), String> {
    let mut types = Types {
        files: BTreeMap::new(),
        imports: BTreeSet::new(),
        aliases: BTreeMap::new(),
    };
    for file in super::list_ts_files(dir)? {
        let name = Path::new(&file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if types.files.insert(name.clone(), file).is_some() {
            return Err(format!("ambiguous binding {name}"));
        }
    }
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tauri_commands");
    let mut commands = BTreeMap::new();
    for entry in std::fs::read_dir(source_dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let file = syn::parse_file(&source).map_err(|e| format!("{}: {e}", path.display()))?;
        types.aliases.clear();
        for item in &file.items {
            if let Item::Use(item) = item {
                imports(&item.tree, "", &mut types.aliases);
            }
        }
        for item in &file.items {
            let Item::Fn(function) = item else { continue };
            let Some(attribute) = function.attrs.iter().find(|attr| {
                attr.path().is_ident("command")
                    || attr
                        .path()
                        .segments
                        .iter()
                        .map(|s| s.ident.to_string())
                        .collect::<Vec<_>>()
                        == ["tauri", "command"]
            }) else {
                continue;
            };
            let mut camel_case = true;
            if let syn::Meta::List(list) = &attribute.meta {
                let options = list
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                    )
                    .map_err(|e| e.to_string())?;
                for option in options {
                    if let syn::Meta::NameValue(value) = option {
                        if value.path.is_ident("rename_all") {
                            let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(value),
                                ..
                            }) = value.value
                            else {
                                return Err("command rename_all must be a string".into());
                            };
                            camel_case = match value.value().as_str() {
                                "camelCase" => true,
                                "snake_case" => false,
                                value => {
                                    return Err(format!(
                                        "unsupported command argument casing {value}"
                                    ))
                                }
                            };
                        }
                    }
                }
            }
            let name = function.sig.ident.to_string();
            let mut arguments = Vec::new();
            for argument in &function.sig.inputs {
                let FnArg::Typed(argument) = argument else {
                    return Err(format!("receiver on command {name}"));
                };
                if types.injected(&argument.ty) {
                    continue;
                }
                let Pat::Ident(pattern) = &*argument.pat else {
                    return Err(format!("destructured command argument {name}"));
                };
                let key = pattern.ident.to_string();
                let key = if camel_case {
                    key.to_lower_camel_case()
                } else {
                    key
                };
                let optional = matches!(&*argument.ty, Type::Path(path) if path.path.segments.last().is_some_and(|s| s.ident == "Option"));
                let value = types
                    .name(&argument.ty)
                    .map_err(|e| format!("{name}.{key}: {e}"))?;
                arguments.push(format!("{key}{}: {value}", if optional { "?" } else { "" }));
            }
            let result = match &function.sig.output {
                ReturnType::Default => "void".into(),
                ReturnType::Type(_, ty) => types.result(ty).map_err(|e| format!("{name}: {e}"))?,
            };
            let args = if arguments.is_empty() {
                "Record<string, never>".into()
            } else {
                format!("{{ {} }}", arguments.join("; "))
            };
            let row = format!("  {name}: {{ args: {args}; result: {result} }};");
            if let Some(previous) = commands.insert(name.clone(), row.clone()) {
                if previous != row {
                    return Err(format!(
                        "conditional command signatures disagree for {name}"
                    ));
                }
            }
        }
    }
    let inventory: BTreeSet<_> = crate::contract::command_inventory()
        .into_iter()
        .map(|command| command.name)
        .collect();
    if commands.keys().cloned().collect::<BTreeSet<_>>() != inventory {
        return Err("native command signatures differ from the command inventory".into());
    }
    let source = format!("// Generated from the native command signatures.\n{}\n\nexport type DesktopCommands = {{\n{}\n}};\n", types.imports.into_iter().collect::<Vec<_>>().join("\n"), commands.into_values().collect::<Vec<_>>().join("\n"));
    std::fs::write(dir.join("DesktopCommands.ts"), source).map_err(|e| e.to_string())
}

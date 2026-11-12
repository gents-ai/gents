use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use serde_json::{Map, Value, json};
use syn::{Attribute, Fields, Item, Type};

pub const ROOT: &str = "LeanContractSnapshot";
pub const GENERATED: &str = "generated.rs";
const SNAPSHOT_FN: &str = "lean_contract_snapshot";
const STRING_SET_LIMIT: usize = 48;

pub struct Source {
    pub path: String,
    pub text: String,
}

pub struct Output {
    pub generated: String,
    pub removals: BTreeMap<String, Vec<(usize, usize)>>,
    pub names: BTreeSet<String>,
    pub report: Value,
}

#[derive(Default, Clone)]
struct Shape {
    seen: u64,
    nulls: u64,
    bools: u64,
    numbers: u64,
    negative: bool,
    float: bool,
    strings: u64,
    values: BTreeSet<String>,
    overflow: bool,
    arrays: u64,
    element: Option<Box<Shape>>,
    objects: u64,
    fields: BTreeMap<String, Shape>,
}

impl Shape {
    fn observe(&mut self, value: &Value, deep: bool) {
        self.seen += 1;
        match value {
            Value::Null => self.nulls += 1,
            Value::Bool(_) => self.bools += 1,
            Value::Number(number) => {
                self.numbers += 1;
                if number.as_u64().is_none() {
                    if number.as_i64().is_some() {
                        self.negative = true;
                    } else {
                        self.float = true;
                    }
                }
            }
            Value::String(text) => {
                self.strings += 1;
                if !self.overflow {
                    self.values.insert(text.clone());
                    if self.values.len() > STRING_SET_LIMIT {
                        self.overflow = true;
                        self.values.clear();
                    }
                }
            }
            Value::Array(items) => {
                self.arrays += 1;
                let element = self.element.get_or_insert_with(Default::default);
                for item in items {
                    element.observe(item, deep);
                }
            }
            Value::Object(map) => {
                self.objects += 1;
                if deep {
                    for (key, child) in map {
                        self.fields.entry(key.clone()).or_default().observe(child, true);
                    }
                }
            }
        }
    }

    fn kinds(&self) -> usize {
        [self.bools, self.numbers, self.strings, self.arrays, self.objects]
            .iter()
            .filter(|count| **count > 0)
            .count()
    }
}

#[derive(Default)]
struct FieldSeen {
    visits: u64,
    present: u64,
    shape: Shape,
    needs_option: bool,
}

#[derive(Default)]
struct TypeSeen {
    visits: u64,
    fields: BTreeMap<String, FieldSeen>,
    variants: BTreeMap<String, u64>,
    unknown: BTreeMap<String, Shape>,
    new_variants: BTreeSet<String>,
}

struct Decoder {
    file: usize,
    item: Item,
    lines: (usize, usize),
    skip: Option<&'static str>,
}

struct Loader {
    file: usize,
    item: syn::ItemFn,
    lines: (usize, usize),
    field: String,
}

struct Oracle {
    paths: Vec<String>,
    uses: Vec<Vec<String>>,
    decoders: BTreeMap<String, Decoder>,
    aliases: BTreeMap<String, Type>,
    loaders: BTreeMap<String, Loader>,
    unit_enums: BTreeMap<String, BTreeSet<String>>,
}

fn serde_args(attrs: &[Attribute]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        let _ = attr.parse_nested_meta(|meta| {
            let key = meta
                .path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_default();
            let value = if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse::<syn::LitStr>()?.value()
            } else if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                content.parse::<proc_macro2::TokenStream>()?;
                String::new()
            } else {
                String::new()
            };
            out.insert(key, value);
            Ok(())
        });
    }
    out
}

fn derives(attrs: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("derive")) {
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(segment) = meta.path.segments.last() {
                out.push(segment.ident.to_string());
            }
            Ok(())
        });
    }
    out
}

fn rename(name: &str, rule: Option<&String>, variant: bool) -> String {
    let Some(rule) = rule else {
        return name.to_owned();
    };
    let words: Vec<String> = if variant {
        let mut words: Vec<String> = Vec::new();
        for ch in name.chars() {
            if ch.is_uppercase() || words.is_empty() {
                words.push(String::new());
            }
            words.last_mut().unwrap().extend(ch.to_lowercase());
        }
        words
    } else {
        name.split('_').map(str::to_owned).collect()
    };
    let capital = |word: &String| {
        let mut chars = word.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    match rule.as_str() {
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),
        "lowercase" => words.concat(),
        "UPPERCASE" => words.concat().to_uppercase(),
        "PascalCase" => words.iter().map(capital).collect(),
        "camelCase" => {
            let mut out = words[0].clone();
            out.extend(words[1..].iter().map(capital));
            out
        }
        other => panic!("unsupported rename_all rule {other}"),
    }
}

fn wire_name(ident: &syn::Ident, attrs: &[Attribute], rule: Option<&String>, variant: bool) -> String {
    let args = serde_args(attrs);
    if let Some(explicit) = args.get("rename") {
        return explicit.clone();
    }
    let name = ident.to_string();
    rename(name.trim_start_matches("r#"), rule, variant)
}

fn peel(ty: &Type) -> &Type {
    match ty {
        Type::Paren(inner) => peel(&inner.elem),
        Type::Group(inner) => peel(&inner.elem),
        other => other,
    }
}

fn head(ty: &Type) -> Option<(String, Vec<&Type>)> {
    let Type::Path(path) = peel(ty) else {
        return None;
    };
    let segment = path.path.segments.last()?;
    let args = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Some((segment.ident.to_string(), args))
}

fn option_inner(ty: &Type) -> Option<&Type> {
    match head(ty) {
        Some((name, args)) if name == "Option" && args.len() == 1 => Some(args[0]),
        _ => None,
    }
}

fn show(ty: &Type) -> String {
    ty.to_token_stream().to_string().replace(' ', "")
}

fn line_of(span: proc_macro2::Span, end: bool) -> usize {
    if end { span.end().line } else { span.start().line }
}

fn first_line(attrs: &[Attribute], fallback: proc_macro2::Span) -> usize {
    attrs
        .iter()
        .map(|attr| line_of(attr.pound_token.span, false))
        .chain([line_of(fallback, false)])
        .min()
        .unwrap()
}

fn flatten_use(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(path) => {
            flatten_use(&path.tree, &format!("{prefix}{}::", path.ident), out)
        }
        syn::UseTree::Name(name) => out.push(format!("{prefix}{}", name.ident)),
        syn::UseTree::Rename(item) => {
            out.push(format!("{prefix}{} as {}", item.ident, item.rename))
        }
        syn::UseTree::Glob(_) => out.push(format!("{prefix}*")),
        syn::UseTree::Group(group) => {
            for item in &group.items {
                flatten_use(item, prefix, out);
            }
        }
    }
}

fn module_of(path: &str) -> Vec<&str> {
    let trimmed = path.trim_end_matches(".rs");
    let mut parts: Vec<&str> = trimmed.split('/').collect();
    if matches!(parts.last(), Some(&"lib") | Some(&"mod")) {
        parts.pop();
    }
    parts
}

fn trivial_loader(function: &syn::ItemFn) -> Option<String> {
    if !function.sig.inputs.is_empty() || function.block.stmts.len() != 1 {
        return None;
    }
    let syn::Stmt::Expr(syn::Expr::Reference(reference), None) = &function.block.stmts[0] else {
        return None;
    };
    let syn::Expr::Field(field) = &*reference.expr else {
        return None;
    };
    let syn::Expr::Call(call) = &*field.base else {
        return None;
    };
    let syn::Expr::Path(callee) = &*call.func else {
        return None;
    };
    let syn::Member::Named(member) = &field.member else {
        return None;
    };
    (callee.path.is_ident(SNAPSHOT_FN) && call.args.is_empty()).then(|| member.to_string())
}

impl Oracle {
    fn parse(sources: &[Source]) -> Self {
        let mut oracle = Oracle {
            paths: Vec::new(),
            uses: Vec::new(),
            decoders: BTreeMap::new(),
            aliases: BTreeMap::new(),
            loaders: BTreeMap::new(),
            unit_enums: BTreeMap::new(),
        };
        let mut duplicates = BTreeSet::new();
        for (index, source) in sources.iter().enumerate() {
            let file = syn::parse_file(&source.text)
                .unwrap_or_else(|error| panic!("{}: {error}", source.path));
            let module = module_of(&source.path);
            let parent = match module.len() {
                0 | 1 => "crate".to_owned(),
                depth => format!("crate::{}", module[..depth - 1].join("::")),
            };
            let mut uses = Vec::new();
            for item in file.items {
                match item {
                    Item::Use(item) if matches!(item.vis, syn::Visibility::Inherited) => {
                        let mut leaves = Vec::new();
                        flatten_use(&item.tree, "", &mut leaves);
                        for leaf in leaves {
                            uses.push(match leaf.strip_prefix("super::") {
                                Some(rest) => format!("{parent}::{rest}"),
                                None => leaf,
                            });
                        }
                    }
                    Item::Type(alias) if alias.generics.params.is_empty() => {
                        oracle.aliases.insert(alias.ident.to_string(), *alias.ty);
                    }
                    Item::Fn(function) => {
                        if let Some(field) = trivial_loader(&function) {
                            let lines = (
                                first_line(&function.attrs, function.sig.fn_token.span),
                                line_of(function.block.brace_token.span.close(), true),
                            );
                            let lines = (
                                lines.0.min(match &function.vis {
                                    syn::Visibility::Public(token) => line_of(token.span, false),
                                    _ => lines.0,
                                }),
                                lines.1,
                            );
                            oracle.loaders.insert(
                                function.sig.ident.to_string(),
                                Loader { file: index, item: function, lines, field },
                            );
                        }
                    }
                    Item::Struct(_) | Item::Enum(_) => {
                        let (ident, attrs, generics, start, end) = match &item {
                            Item::Struct(item) => (
                                &item.ident,
                                &item.attrs,
                                &item.generics,
                                item.struct_token.span,
                                match (&item.fields, &item.semi_token) {
                                    (Fields::Named(fields), _) => fields.brace_token.span.close(),
                                    (_, Some(semi)) => semi.span,
                                    _ => item.struct_token.span,
                                },
                            ),
                            Item::Enum(item) => (
                                &item.ident,
                                &item.attrs,
                                &item.generics,
                                item.enum_token.span,
                                item.brace_token.span.close(),
                            ),
                            _ => unreachable!(),
                        };
                        if !derives(attrs).iter().any(|name| name == "Deserialize") {
                            continue;
                        }
                        let skip = if !generics.params.is_empty() {
                            Some("generic type")
                        } else if attrs.iter().any(|attr| attr.path().is_ident("cfg")) {
                            Some("cfg-gated type")
                        } else {
                            None
                        };
                        let name = ident.to_string();
                        if let Item::Enum(item) = &item {
                            let args = serde_args(&item.attrs);
                            if item.variants.iter().all(|v| matches!(v.fields, Fields::Unit))
                                && !args.contains_key("tag")
                            {
                                let names = item
                                    .variants
                                    .iter()
                                    .map(|v| {
                                        wire_name(&v.ident, &v.attrs, args.get("rename_all"), true)
                                    })
                                    .collect();
                                oracle.unit_enums.insert(name.clone(), names);
                            }
                        }
                        let lines = (first_line(attrs, start), line_of(end, true));
                        let decoder = Decoder { file: index, item, lines, skip };
                        if oracle.decoders.insert(name.clone(), decoder).is_some() {
                            duplicates.insert(name);
                        }
                    }
                    _ => {}
                }
            }
            oracle.paths.push(source.path.clone());
            oracle.uses.push(uses);
        }
        for name in duplicates {
            oracle.decoders.get_mut(&name).unwrap().skip = Some("name defined twice");
        }
        oracle
    }

    fn reach(&self, ty: &Type, out: &mut BTreeSet<String>) {
        match peel(ty) {
            Type::Tuple(tuple) => tuple.elems.iter().for_each(|elem| self.reach(elem, out)),
            Type::Array(array) => self.reach(&array.elem, out),
            Type::Slice(slice) => self.reach(&slice.elem, out),
            Type::Reference(reference) => self.reach(&reference.elem, out),
            other => {
                let Some((name, args)) = head(other) else {
                    return;
                };
                args.iter().for_each(|arg| self.reach(arg, out));
                if let Some(alias) = self.aliases.get(&name) {
                    return self.reach(alias, out);
                }
                let Some(decoder) = self.decoders.get(&name) else {
                    return;
                };
                if decoder.skip.is_some() || !out.insert(name) {
                    return;
                }
                let mut visit = |fields: &Fields| {
                    fields.iter().for_each(|field| self.reach(&field.ty, out))
                };
                match &decoder.item {
                    Item::Struct(item) => visit(&item.fields),
                    Item::Enum(item) => item.variants.iter().for_each(|v| visit(&v.fields)),
                    _ => {}
                }
            }
        }
    }
}

struct Walker<'a> {
    oracle: &'a Oracle,
    seen: BTreeMap<String, TypeSeen>,
    untyped: BTreeMap<String, Shape>,
    external: BTreeSet<String>,
    mismatches: BTreeMap<String, u64>,
    dry: u32,
}

impl Walker<'_> {
    fn fail(&mut self, at: &str, why: String) -> bool {
        if self.dry == 0 {
            *self.mismatches.entry(format!("{at}: {why}")).or_default() += 1;
        }
        false
    }

    fn walk(&mut self, ty: &Type, value: &Value, at: &str) -> bool {
        match peel(ty) {
            Type::Tuple(tuple) => {
                let Some(items) = value.as_array().filter(|a| a.len() == tuple.elems.len()) else {
                    return self.fail(at, format!("expected {}-tuple", tuple.elems.len()));
                };
                let mut ok = true;
                for (elem, item) in tuple.elems.iter().zip(items) {
                    ok &= self.walk(elem, item, at);
                }
                ok
            }
            Type::Array(array) => self.walk_list(&array.elem, value, at),
            other => {
                let Some((name, args)) = head(other) else {
                    return self.fail(at, format!("unsupported type {}", show(ty)));
                };
                self.walk_named(&name, &args, value, at)
            }
        }
    }

    fn walk_list(&mut self, elem: &Type, value: &Value, at: &str) -> bool {
        let Some(items) = value.as_array() else {
            return self.fail(at, "expected array".into());
        };
        let mut ok = true;
        for item in items {
            ok &= self.walk(elem, item, at);
        }
        ok
    }

    fn walk_named(&mut self, name: &str, args: &[&Type], value: &Value, at: &str) -> bool {
        let unsigned = |max: u64| value.as_u64().is_some_and(|n| n <= max);
        let ok = match (name, args.len()) {
            ("Option", 1) => return value.is_null() || self.walk(args[0], value, at),
            ("Box" | "Arc" | "Rc", 1) => return self.walk(args[0], value, at),
            ("Vec" | "BTreeSet" | "HashSet" | "RapidHashSet" | "VecDeque", 1) => {
                return self.walk_list(args[0], value, at);
            }
            ("BTreeMap" | "HashMap" | "RapidHashMap" | "IndexMap", 2) => {
                let Some(map) = value.as_object() else {
                    return self.fail(at, "expected object".into());
                };
                let mut ok = true;
                for child in map.values() {
                    ok &= self.walk(args[1], child, at);
                }
                return ok;
            }
            ("bool", 0) => value.is_boolean(),
            ("u8", 0) => unsigned(u8::MAX.into()),
            ("u16", 0) => unsigned(u16::MAX.into()),
            ("u32", 0) => unsigned(u32::MAX.into()),
            ("u64" | "usize" | "u128", 0) => value.is_u64(),
            ("i8" | "i16" | "i32" | "i64" | "isize", 0) => value.is_i64(),
            ("f32" | "f64", 0) => value.is_number(),
            ("String", 0) => value.is_string(),
            ("Value", 0) => {
                if self.dry == 0 {
                    self.untyped.entry(at.to_owned()).or_default().observe(value, true);
                }
                true
            }
            _ => {
                let oracle = self.oracle;
                if let Some(alias) = oracle.aliases.get(name) {
                    return self.walk(alias, value, at);
                }
                return match oracle.decoders.get(name) {
                    Some(decoder) if decoder.skip.is_none() => {
                        self.walk_decoder(name, &decoder.item, value, None)
                    }
                    _ => {
                        self.external.insert(name.to_owned());
                        true
                    }
                };
            }
        };
        ok || self.fail(at, format!("{value} is not {name}"))
    }

    fn walk_decoder(&mut self, name: &str, item: &Item, value: &Value, extra: Option<&str>) -> bool {
        if self.dry == 0 {
            self.seen.entry(name.to_owned()).or_default().visits += 1;
        }
        match item {
            Item::Struct(item) => {
                let args = serde_args(&item.attrs);
                match &item.fields {
                    Fields::Named(_) => {
                        let Some(map) = value.as_object() else {
                            return self.fail(name, "expected object".into());
                        };
                        self.walk_fields(name, "", &item.fields, &args, map, extra)
                    }
                    Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                        self.walk(&fields.unnamed[0].ty, value, name)
                    }
                    _ => self.fail(name, "unsupported struct form".into()),
                }
            }
            Item::Enum(item) => self.walk_enum(name, item, value),
            _ => unreachable!(),
        }
    }

    fn walk_enum(&mut self, name: &str, item: &syn::ItemEnum, value: &Value) -> bool {
        let args = serde_args(&item.attrs);
        let rule = args.get("rename_all");
        let wire = |variant: &syn::Variant| wire_name(&variant.ident, &variant.attrs, rule, true);
        if args.contains_key("untagged") {
            for variant in &item.variants {
                self.dry += 1;
                let fits = self.walk_variant(name, variant, &args, value, None);
                self.dry -= 1;
                if fits {
                    self.count_variant(name, &variant.ident.to_string());
                    return self.walk_variant(name, variant, &args, value, None);
                }
            }
            return self.fail(name, "no untagged variant fits".into());
        }
        if let Some(tag) = args.get("tag") {
            let Some(found) = value.get(tag).and_then(Value::as_str) else {
                return self.fail(name, format!("missing tag {tag}"));
            };
            let Some(variant) = item.variants.iter().find(|v| wire(v) == found) else {
                return self.fail(name, format!("unknown {tag} {found:?}"));
            };
            self.count_variant(name, &variant.ident.to_string());
            return self.walk_variant(name, variant, &args, value, Some(tag));
        }
        let (found, content) = match value {
            Value::String(text) => (text.as_str(), &Value::Null),
            Value::Object(map) if map.len() == 1 => {
                let (key, content) = map.iter().next().unwrap();
                (key.as_str(), content)
            }
            _ => return self.fail(name, "expected enum".into()),
        };
        let Some(variant) = item.variants.iter().find(|v| wire(v) == found) else {
            if self.dry == 0 && value.is_string() {
                let seen = self.seen.entry(name.to_owned()).or_default();
                seen.new_variants.insert(found.to_owned());
            }
            return self.fail(name, format!("unknown variant {found:?}"));
        };
        self.count_variant(name, &variant.ident.to_string());
        match (&variant.fields, content) {
            (Fields::Unit, Value::Null) => true,
            (Fields::Unit, _) => self.fail(name, format!("{found} carries content")),
            _ => self.walk_variant(name, variant, &args, content, None),
        }
    }

    fn count_variant(&mut self, name: &str, variant: &str) {
        if self.dry == 0 {
            let seen = self.seen.entry(name.to_owned()).or_default();
            *seen.variants.entry(variant.to_owned()).or_default() += 1;
        }
    }

    fn walk_variant(
        &mut self,
        name: &str,
        variant: &syn::Variant,
        container: &BTreeMap<String, String>,
        value: &Value,
        tag: Option<&str>,
    ) -> bool {
        let label = format!("{}.", variant.ident);
        match &variant.fields {
            Fields::Unit => true,
            Fields::Named(_) => {
                let Some(map) = value.as_object() else {
                    return self.fail(name, format!("{label} expected object"));
                };
                let mut args = container.clone();
                args.extend(serde_args(&variant.attrs));
                self.walk_fields(name, &label, &variant.fields, &args, map, tag)
            }
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                let ty = &fields.unnamed[0].ty;
                let inner = head(ty).and_then(|(inner, _)| {
                    let decoder = self.oracle.decoders.get(&inner)?;
                    decoder.skip.is_none().then_some((inner, &decoder.item))
                });
                match (inner, tag) {
                    (Some((inner, item)), Some(_)) => self.walk_decoder(&inner, item, value, tag),
                    _ => self.walk(ty, value, name),
                }
            }
            Fields::Unnamed(fields) => {
                let types: Vec<&Type> = fields.unnamed.iter().map(|field| &field.ty).collect();
                let Some(items) = value.as_array().filter(|a| a.len() == types.len()) else {
                    return self.fail(name, format!("{label} expected tuple"));
                };
                let mut ok = true;
                for (ty, item) in types.iter().zip(items) {
                    ok &= self.walk(ty, item, name);
                }
                ok
            }
        }
    }

    fn flattened_keys(&self, ty: &Type, out: &mut BTreeSet<String>) {
        let Some((name, _)) = head(ty) else { return };
        let Some(Decoder { item: Item::Struct(item), .. }) = self.oracle.decoders.get(&name) else {
            return;
        };
        let args = serde_args(&item.attrs);
        for field in &item.fields {
            if serde_args(&field.attrs).contains_key("flatten") {
                self.flattened_keys(&field.ty, out);
            } else if let Some(ident) = &field.ident {
                out.insert(wire_name(ident, &field.attrs, args.get("rename_all"), false));
            }
        }
    }

    fn walk_fields(
        &mut self,
        owner: &str,
        label: &str,
        fields: &Fields,
        container: &BTreeMap<String, String>,
        map: &Map<String, Value>,
        extra: Option<&str>,
    ) -> bool {
        let mut ok = true;
        let mut known: BTreeSet<String> = extra.iter().map(|key| (*key).to_owned()).collect();
        let mut flattens = false;
        for field in fields {
            let ident = field.ident.as_ref().expect("named field");
            let args = serde_args(&field.attrs);
            if args.contains_key("flatten") {
                flattens = true;
                self.flattened_keys(&field.ty, &mut known);
                self.dry += 1;
                let fits = self.walk(&field.ty, &Value::Object(map.clone()), owner);
                self.dry -= 1;
                if !fits {
                    ok = self.fail(owner, format!("{label}{ident} flattened struct does not fit"));
                }
                continue;
            }
            let wire = wire_name(ident, &field.attrs, container.get("rename_all"), false);
            let at = format!("{owner}.{label}{ident}");
            let custom = args.get("deserialize_with").map(|path| {
                path.rsplit("::").next().unwrap_or_default().to_owned()
            });
            let optional = option_inner(&field.ty).is_some();
            let child = map.get(&wire);
            if self.dry == 0 {
                let seen = self.seen.entry(owner.to_owned()).or_default();
                let seen = seen.fields.entry(format!("{label}{ident}")).or_default();
                seen.visits += 1;
                if let Some(child) = child {
                    seen.present += 1;
                    seen.shape.observe(child, false);
                }
            }
            known.insert(wire.clone());
            let defaulted = args.contains_key("default") || container.contains_key("default");
            let fits = match (child, custom.as_deref()) {
                (None, None) if optional => true,
                (None, _) if defaulted => true,
                (None, _) => false,
                (Some(Value::Null), None) if !optional && !show(&field.ty).ends_with("Value") => false,
                (Some(child), None | Some("required_nullable")) => {
                    ok &= self.walk(&field.ty, child, &at);
                    true
                }
                (Some(child), Some("ordered_pairs")) => {
                    let pair = head(&field.ty).and_then(|(_, args)| match args.first().map(|t| peel(t)) {
                        Some(Type::Tuple(tuple)) if tuple.elems.len() == 2 => Some(&tuple.elems[1]),
                        _ => None,
                    });
                    match (pair, child.as_object()) {
                        (Some(ty), Some(entries)) => {
                            for entry in entries.values() {
                                ok &= self.walk(ty, entry, &at);
                            }
                        }
                        _ => ok = self.fail(&at, "ordered_pairs expects an object".into()),
                    }
                    true
                }
                (Some(_), Some(other)) => {
                    self.external.insert(format!("deserialize_with {other}"));
                    true
                }
            };
            if !fits {
                if self.dry == 0 {
                    let seen = self.seen.get_mut(owner).unwrap();
                    seen.fields.get_mut(&format!("{label}{ident}")).unwrap().needs_option = true;
                }
                ok = self.fail(&at, "missing or null but required".into());
            }
        }
        let strict = container.contains_key("deny_unknown_fields");
        for (key, child) in map {
            if known.contains(key) || (flattens && !strict) {
                continue;
            }
            if self.dry == 0 {
                let seen = self.seen.entry(owner.to_owned()).or_default();
                seen.unknown.entry(format!("{label}{key}")).or_default().observe(child, true);
            }
            if strict {
                ok = self.fail(owner, format!("{label}{key} is not decoded by a strict type"));
            }
        }
        ok
    }
}

fn camel(name: &str) -> String {
    name.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().unwrap().to_uppercase();
            first.chain(chars).collect::<String>()
        })
        .collect()
}

fn is_field_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_lowercase() || ch == '_')
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "type", "unsafe", "use", "where", "while", "box", "final", "override",
];

struct Inference<'a> {
    unit_enums: &'a BTreeMap<String, BTreeSet<String>>,
    taken: BTreeSet<String>,
    structs: BTreeMap<String, String>,
}

impl Inference<'_> {
    fn exact_enum(&self, shape: &Shape) -> Option<&String> {
        if shape.overflow || shape.values.len() < 2 {
            return None;
        }
        let mut matches = self.unit_enums.iter().filter(|(_, names)| **names == shape.values);
        let (name, _) = matches.next()?;
        matches.next().is_none().then_some(name)
    }

    fn infer(&mut self, shape: &Shape, name: &str) -> String {
        let inner = match shape.kinds() {
            0 => "serde_json::Value".to_owned(),
            1 if shape.bools > 0 => "bool".to_owned(),
            1 if shape.numbers > 0 => match (shape.float, shape.negative) {
                (true, _) => "f64".to_owned(),
                (false, true) => "i64".to_owned(),
                (false, false) => "u64".to_owned(),
            },
            1 if shape.strings > 0 => self.exact_enum(shape).cloned().unwrap_or("String".into()),
            1 if shape.arrays > 0 => {
                let element = shape.element.as_deref().cloned().unwrap_or_default();
                let singular = name.strip_suffix("Cases").map_or(format!("{name}Item"), |stem| {
                    format!("{stem}Case")
                });
                let mut inner = self.infer(&element, &singular);
                if element.nulls > 0 {
                    inner = format!("Option<{inner}>");
                }
                format!("Vec<{inner}>")
            }
            1 => self.infer_object(shape, name),
            _ => "serde_json::Value".to_owned(),
        };
        inner
    }

    fn infer_object(&mut self, shape: &Shape, name: &str) -> String {
        if shape.fields.is_empty() {
            return "serde_json::Value".to_owned();
        }
        if !shape.fields.keys().all(|key| is_field_name(key)) {
            let mut merged = Shape::default();
            for child in shape.fields.values() {
                merge(&mut merged, child);
            }
            let inner = self.infer(&merged, &format!("{name}Entry"));
            return format!("BTreeMap<String, {inner}>");
        }
        let mut name = name.to_owned();
        while !self.taken.insert(name.clone()) {
            name.push('_');
        }
        let mut body = String::new();
        for (key, child) in &shape.fields {
            let mut ty = self.infer(child, &format!("{name}{}", camel(key)));
            if child.nulls > 0 || child.seen < shape.objects {
                ty = format!("Option<{ty}>");
            }
            let raw = if KEYWORDS.contains(&key.as_str()) { "r#" } else { "" };
            body.push_str(&format!("pub {raw}{key}: {ty},"));
        }
        self.structs.insert(
            name.clone(),
            format!(
                "#[derive(Debug, Clone, PartialEq, Deserialize)] #[serde(deny_unknown_fields)] \
                 pub struct {name} {{ {body} }}"
            ),
        );
        name
    }
}

fn merge(into: &mut Shape, from: &Shape) {
    into.seen += from.seen;
    into.nulls += from.nulls;
    into.bools += from.bools;
    into.numbers += from.numbers;
    into.negative |= from.negative;
    into.float |= from.float;
    into.strings += from.strings;
    into.overflow |= from.overflow;
    into.values.extend(from.values.iter().cloned());
    if into.overflow || into.values.len() > STRING_SET_LIMIT {
        into.overflow = true;
        into.values.clear();
    }
    into.arrays += from.arrays;
    if let Some(element) = &from.element {
        merge(into.element.get_or_insert_with(Default::default), element);
    }
    into.objects += from.objects;
    for (key, child) in &from.fields {
        merge(into.fields.entry(key.clone()).or_default(), child);
    }
}

fn disagreements(
    oracle: &Oracle,
    inference: &Inference,
    ty: &Type,
    args: &BTreeMap<String, String>,
    seen: &FieldSeen,
) -> Vec<String> {
    let mut out = Vec::new();
    if seen.present == 0 {
        return vec!["never_emitted".into()];
    }
    let optional = option_inner(ty);
    let sometimes_absent = seen.present < seen.visits || seen.shape.nulls > 0;
    if optional.is_some() && !sometimes_absent {
        out.push("option_always_present".into());
    }
    if optional.is_none() && args.contains_key("default") && !sometimes_absent {
        out.push("default_always_present".into());
    }
    if seen.shape.nulls == seen.shape.seen {
        out.push("always_null".into());
        return out;
    }
    let mut ty = optional.unwrap_or(ty);
    let mut shape = &seen.shape;
    loop {
        let Some((name, type_args)) = head(ty) else { break };
        match (name.as_str(), type_args.len()) {
            ("Vec" | "BTreeSet", 1) => {
                match shape.element.as_deref().filter(|element| element.seen > 0) {
                    Some(element) => {
                        shape = element;
                        ty = option_inner(type_args[0]).unwrap_or(type_args[0]);
                        continue;
                    }
                    None => out.push("list_always_empty".into()),
                }
            }
            ("String", 0) => {
                if let Some(found) = inference.exact_enum(shape) {
                    out.push(format!("string_is_enum {found}"));
                }
            }
            ("i8" | "i16" | "i32" | "i64" | "isize", 0) if !shape.negative => {
                out.push("signed_never_negative".into())
            }
            ("f32" | "f64", 0) if !shape.float => out.push("float_always_integral".into()),
            ("Value", 0) => out.push("untyped_value".into()),
            _ => {}
        }
        break;
    }
    let _ = oracle;
    out
}

fn bump(vis: &mut syn::Visibility) {
    if matches!(vis, syn::Visibility::Inherited) {
        *vis = syn::parse_quote!(pub(crate));
    }
}

fn patch_fields(
    fields: &mut Fields,
    label: &str,
    seen: Option<&TypeSeen>,
    strict: bool,
    owner: &str,
    inference: &mut Inference,
    patches: &mut Vec<String>,
) {
    let Fields::Named(named) = fields else { return };
    for field in &mut named.named {
        bump(&mut field.vis);
        let ident = field.ident.as_ref().unwrap().to_string();
        let needs = seen
            .and_then(|seen| seen.fields.get(&format!("{label}{ident}")))
            .is_some_and(|field| field.needs_option);
        if needs && option_inner(&field.ty).is_none() {
            let ty = &field.ty;
            field.ty = syn::parse_quote!(Option<#ty>);
            patches.push(format!("{owner}.{label}{ident}: wrapped in Option"));
        }
    }
    let Some(seen) = seen.filter(|_| strict) else { return };
    for (key, shape) in &seen.unknown {
        let Some(key) = key.strip_prefix(label).filter(|key| is_field_name(key)) else {
            continue;
        };
        if label.is_empty() && key.contains('.') {
            continue;
        }
        let default = match owner {
            ROOT => format!("Lean{}", camel(key)),
            _ => format!("{owner}{}", camel(key)),
        };
        let mut ty = inference.infer(shape, &default);
        if shape.nulls > 0 || shape.seen < seen.visits {
            ty = format!("Option<{ty}>");
        }
        let raw = if KEYWORDS.contains(&key) { "r#" } else { "" };
        let field: syn::Field = syn::parse::Parser::parse_str(
            syn::Field::parse_named,
            &format!("pub {raw}{key}: {ty}"),
        )
        .unwrap();
        named.named.push(field);
        patches.push(format!("{owner}.{label}{key}: added as {ty}"));
    }
}

pub fn generate(snapshot: &Value, sources: &[Source], only: Option<&BTreeSet<String>>) -> Output {
    let oracle = Oracle::parse(sources);
    let root = oracle.decoders.get(ROOT).expect("oracle defines the snapshot root");
    let Item::Struct(root_item) = &root.item else {
        panic!("snapshot root is a struct")
    };

    let mut walker = Walker {
        oracle: &oracle,
        seen: BTreeMap::new(),
        untyped: BTreeMap::new(),
        external: BTreeSet::new(),
        mismatches: BTreeMap::new(),
        dry: 0,
    };
    walker.walk_decoder(ROOT, &root.item, snapshot, None);
    let Walker { seen, untyped, external, mismatches, .. } = walker;

    let mut emitted = BTreeSet::from([ROOT.to_owned()]);
    let mut all_reached = BTreeSet::from([ROOT.to_owned()]);
    let mut typed_keys = BTreeSet::new();
    let mut untyped_keys = BTreeSet::new();
    for field in &root_item.fields {
        let key = field.ident.as_ref().unwrap().to_string();
        let mut reached = BTreeSet::new();
        oracle.reach(&field.ty, &mut reached);
        if show(&field.ty).contains("Value") {
            untyped_keys.insert(key.clone());
        } else {
            typed_keys.insert(key.clone());
        }
        if only.is_none_or(|only| only.contains(&key)) {
            emitted.extend(reached.iter().cloned());
        }
        all_reached.extend(reached);
    }
    if only.is_some() {
        for (name, decoder) in &oracle.decoders {
            if oracle.paths[decoder.file] == GENERATED {
                emitted.insert(name.clone());
            }
        }
    }

    let mut inference = Inference {
        unit_enums: &oracle.unit_enums,
        taken: oracle.decoders.keys().cloned().collect(),
        structs: BTreeMap::new(),
    };
    let mut patches = Vec::new();
    let mut findings: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut fields_total = 0u64;
    let mut fields_agree = 0u64;
    let mut items = Vec::new();
    let mut removals: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut uses = BTreeSet::from(["crate::*".to_owned(), "serde::Deserialize".to_owned()]);
    let mut origin_files = BTreeSet::new();

    for name in &emitted {
        let decoder = &oracle.decoders[name];
        let type_seen = seen.get(name);
        let mut item = decoder.item.clone();
        let mut note = |fields: &Fields, label: &str, findings: &mut BTreeMap<String, Vec<String>>| {
            for field in fields {
                let Some(ident) = &field.ident else { continue };
                if name == ROOT {
                    continue;
                }
                fields_total += 1;
                let empty = FieldSeen::default();
                let field_seen = type_seen
                    .and_then(|seen| seen.fields.get(&format!("{label}{ident}")))
                    .unwrap_or(&empty);
                let found = disagreements(
                    &oracle,
                    &inference,
                    &field.ty,
                    &serde_args(&field.attrs),
                    field_seen,
                );
                if found.is_empty() {
                    fields_agree += 1;
                }
                for class in found {
                    let (class, detail) = class.split_once(' ').unwrap_or((&class, ""));
                    findings.entry(class.to_owned()).or_default().push(
                        format!("{name}.{label}{ident}: {} {detail}", show(&field.ty))
                            .trim()
                            .to_owned(),
                    );
                }
            }
        };
        match &decoder.item {
            Item::Struct(item) => note(&item.fields, "", &mut findings),
            Item::Enum(item) => {
                for variant in &item.variants {
                    let count = type_seen.and_then(|seen| seen.variants.get(&variant.ident.to_string()));
                    if count.is_none() {
                        findings
                            .entry("variant_never_emitted".into())
                            .or_default()
                            .push(format!("{name}::{}", variant.ident));
                    } else {
                        note(&variant.fields, &format!("{}.", variant.ident), &mut findings);
                    }
                }
            }
            _ => {}
        }
        if let Some(type_seen) = type_seen {
            let strict = match &decoder.item {
                Item::Struct(item) => serde_args(&item.attrs).contains_key("deny_unknown_fields"),
                Item::Enum(item) => serde_args(&item.attrs).contains_key("deny_unknown_fields"),
                _ => false,
            };
            if !strict {
                for key in type_seen.unknown.keys() {
                    findings
                        .entry("emitted_but_not_decoded".into())
                        .or_default()
                        .push(format!("{name}.{key}"));
                }
            }
        }
        match &mut item {
            Item::Struct(item) => {
                bump(&mut item.vis);
                let strict = serde_args(&item.attrs).contains_key("deny_unknown_fields");
                patch_fields(&mut item.fields, "", type_seen, strict, name, &mut inference, &mut patches);
                if let Fields::Unnamed(fields) = &mut item.fields {
                    fields.unnamed.iter_mut().for_each(|field| bump(&mut field.vis));
                }
            }
            Item::Enum(item) => {
                bump(&mut item.vis);
                let args = serde_args(&item.attrs);
                let strict = args.contains_key("deny_unknown_fields");
                for variant in &mut item.variants {
                    let label = format!("{}.", variant.ident);
                    let mut scratch = Vec::new();
                    let mut fields = variant.fields.clone();
                    patch_fields(&mut fields, &label, type_seen, strict, name, &mut inference, &mut scratch);
                    if !scratch.is_empty() {
                        if let (Fields::Named(patched), Fields::Named(original)) =
                            (&mut fields, &variant.fields)
                        {
                            for (field, before) in patched.named.iter_mut().zip(&original.named) {
                                field.vis = before.vis.clone();
                            }
                            for field in patched.named.iter_mut().skip(original.named.len()) {
                                field.vis = syn::Visibility::Inherited;
                            }
                        }
                        variant.fields = fields;
                        patches.extend(scratch);
                    }
                }
                for wire in type_seen.map(|seen| &seen.new_variants).into_iter().flatten() {
                    let ident = quote::format_ident!("{}", camel(wire));
                    let variant: syn::Variant = syn::parse_quote!(#[serde(rename = #wire)] #ident);
                    item.variants.push(variant);
                    patches.push(format!("{name}::{ident}: added for {wire:?}"));
                }
            }
            _ => {}
        }
        items.push(item);
        let path = &oracle.paths[decoder.file];
        origin_files.insert(decoder.file);
        if path != GENERATED {
            removals.entry(path.clone()).or_default().push(decoder.lines);
        }
    }

    let items_emitted = emitted.len();
    let mut loaders = Vec::new();
    for (name, loader) in &oracle.loaders {
        if !only.is_none_or(|only| only.contains(&loader.field))
            && oracle.paths[loader.file] != GENERATED
        {
            continue;
        }
        loaders.push(Item::Fn(loader.item.clone()));
        origin_files.insert(loader.file);
        let path = &oracle.paths[loader.file];
        if path != GENERATED {
            removals.entry(path.clone()).or_default().push(loader.lines);
        }
        emitted.insert(name.clone());
    }
    let loaders_emitted = loaders.len();
    for file in origin_files {
        uses.extend(oracle.uses[file].iter().cloned());
    }
    uses.remove("crate::generated::*");

    let mut text = String::from("#![allow(unused_imports)]\n\n");
    for leaf in &uses {
        text.push_str(&format!("use {leaf};\n"));
    }
    let inferred: Vec<Item> = inference
        .structs
        .values()
        .map(|body| syn::parse_str(body).unwrap_or_else(|error| panic!("{body}: {error}")))
        .collect();
    let file = syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: loaders.into_iter().chain(items).chain(inferred).collect(),
    };
    let body = prettyplease::unparse(&file);
    let mut previous_closed = false;
    for line in body.lines() {
        let opens = !line.starts_with(' ') && !line.starts_with('}') && !line.is_empty();
        if previous_closed && opens {
            text.push('\n');
        }
        if opens && text.ends_with(";\n") && !line.starts_with("use ") && !text.ends_with("\n\n") {
            text.push('\n');
        }
        text.push_str(line);
        text.push('\n');
        previous_closed = line == "}" || (!line.starts_with(' ') && line.ends_with(';'));
    }

    let not_covered: BTreeMap<&String, String> = oracle
        .decoders
        .iter()
        .filter(|(name, _)| !all_reached.contains(*name))
        .map(|(name, decoder)| {
            let reason = decoder.skip.unwrap_or("not reachable from the typed snapshot root");
            (name, format!("{} ({})", reason, oracle.paths[decoder.file]))
        })
        .collect();
    let mut untyped_inference = Inference {
        unit_enums: &oracle.unit_enums,
        taken: oracle.decoders.keys().cloned().collect(),
        structs: BTreeMap::new(),
    };
    for (at, shape) in &untyped {
        let key = at.rsplit('.').next().unwrap_or(at);
        untyped_inference.infer(shape, &format!("Lean{}", camel(key)));
    }
    let report = json!({
        "snapshot_keys": snapshot.as_object().map_or(0, Map::len),
        "typed_keys": typed_keys.len(),
        "untyped_keys": untyped_keys,
        "untyped_positions": untyped.len(),
        "untyped_structs_inferable": untyped_inference.structs.len(),
        "decoder_types": oracle.decoders.len(),
        "types_emitted": items_emitted,
        "loaders_emitted": loaders_emitted,
        "fields_checked": fields_total,
        "fields_agree": fields_agree,
        "findings": findings,
        "patches": patches,
        "mismatches": mismatches,
        "not_covered": not_covered,
        "external_types": external,
    });
    Output { generated: text, removals, names: emitted, report }
}

pub fn prune(text: &str, ranges: &[(usize, usize)], moved: &BTreeSet<String>) -> Option<String> {
    let mut drop = BTreeSet::new();
    for (start, end) in ranges {
        drop.extend(*start..=*end);
    }
    let mut out: Vec<&str> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if drop.contains(&(index + 1)) || (line.is_empty() && out.last().is_some_and(|l| l.is_empty())) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    let rest = syn::parse_file(&out.join("\n")).expect("pruned file parses");
    if rest.items.iter().all(|item| matches!(item, Item::Use(_))) {
        return None;
    }
    let globbed = out.iter().any(|line| ["use super::*;", "use crate::*;"].contains(line));
    let refers = out.iter().any(|line| {
        line.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .any(|word| moved.contains(word))
    });
    if refers && !globbed {
        let at = out
            .iter()
            .rposition(|line| line.starts_with("use ") && line.ends_with(';'))
            .map_or(0, |index| index + 1);
        out.insert(at, "use crate::generated::*;");
    }
    Some(out.join("\n") + "\n")
}

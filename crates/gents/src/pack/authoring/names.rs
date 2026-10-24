#[derive(Debug, thiserror::Error)]
pub enum NamesError {
    #[error("pack names are snake_case; use {snake:?}")]
    NotSnakeCase { snake: String },
}

type Result<T, E = NamesError> = std::result::Result<T, E>;

#[derive(Clone, Debug)]
pub struct Names {
    pub name: String,
    pub namespace: String,
    pub kebab: String,
    pub title: String,
}

impl Names {
    pub fn new(name: &str, namespace: &str) -> Result<Self> {
        let snake = to_snake_case(name);
        if snake != name || name.is_empty() {
            return Err(NamesError::NotSnakeCase { snake });
        }
        Ok(Self {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kebab: name.replace('_', "-"),
            title: name
                .split('_')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect(),
        })
    }
}

pub fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

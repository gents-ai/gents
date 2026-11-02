use super::{GraphqlError, Result};

/// Validate `name` against the GraphQL `Name` grammar:
/// `[_A-Za-z][_0-9A-Za-z]*` (ASCII only). Anything interpolated into a
/// GraphQL document in identifier position MUST pass this check first —
/// escaping does not exist for identifiers.
pub fn validate_graphql_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        Some(first) => {
            return Err(GraphqlError::IdentifierStart {
                name: name.to_owned(),
                first,
            });
        }
        None => return Err(GraphqlError::EmptyIdentifier),
    }
    if let Some(character) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        return Err(GraphqlError::IdentifierCharacter {
            name: name.to_owned(),
            character,
        });
    }
    Ok(())
}

/// Validate a value used as a **collection name** in identifier position
/// (e.g. an event source's collection name). On top of the Name grammar this
/// rejects the `__` prefix, which the GraphQL spec reserves for
/// introspection — a "collection" of `__Type` or `__schema` would aim a
/// query at the introspection surface instead of a document collection.
pub fn validate_collection_identifier(name: &str) -> Result<()> {
    validate_graphql_name(name)?;
    if name.starts_with("__") {
        return Err(GraphqlError::ReservedCollectionPrefix {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Validate a caller-supplied GraphQL **filter-object fragment** — a value
/// spliced into a query whole, as an event-source filter is.
///
/// This is the third interpolation position, and neither of the other two
/// defenses reaches it: escaping would destroy the object syntax, and the
/// value is not an identifier. What makes it checkable is that a break-out
/// must unbalance the fragment — to escape it has to close a `]`, `}` or
/// `)` it never opened. So: require one balanced object literal, built only
/// from the tokens a filter legitimately needs.
///
/// Deliberately strict. `(`, `)` and `#` never appear in a filter object,
/// but each is load-bearing for an attack (`)` closes the enclosing field's
/// argument list, `#` comments out the query's own tail), so they are
/// rejected outright rather than balance-tracked.
pub fn validate_graphql_filter_fragment(filter: &str) -> Result<()> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return Err(GraphqlError::EmptyFilter);
    }
    if !trimmed.starts_with('{') {
        return Err(GraphqlError::FilterNotObject);
    }

    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut closed_at: Option<usize> = None;
    let mut after_string = false;

    for (index, ch) in trimmed.char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => {
                    in_string = false;
                    after_string = true;
                }
                _ => {}
            }
            continue;
        }
        // GraphQL object keys are names; a JSON-style quoted key parses as a
        // string here but fails when the runtime queries with the filter.
        if after_string && !ch.is_whitespace() {
            after_string = false;
            if ch == ':' {
                return Err(GraphqlError::FilterQuotedKey);
            }
        }
        if closed_at.is_some() && !ch.is_whitespace() {
            return Err(GraphqlError::FilterTrailingContent);
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => stack.push(ch),
            '}' | ']' => {
                let opener = stack.pop().ok_or(GraphqlError::FilterUnbalanced {
                    character: ch,
                    byte: index,
                })?;
                let expected = if ch == '}' { '{' } else { '[' };
                if opener != expected {
                    return Err(GraphqlError::FilterMismatched {
                        opener,
                        closer: ch,
                        byte: index,
                    });
                }
                if stack.is_empty() {
                    closed_at = Some(index);
                }
            }
            ':' | ',' => {}
            c if c.is_ascii_alphanumeric() || c == '_' => {}
            '-' | '+' | '.' => {}
            c if c.is_whitespace() => {}
            c => {
                return Err(GraphqlError::FilterCharacter {
                    character: c,
                    byte: index,
                });
            }
        }
    }

    if in_string {
        return Err(GraphqlError::FilterUnterminatedString);
    }
    if !stack.is_empty() {
        return Err(GraphqlError::FilterUnclosed {
            opener: stack.last().copied(),
        });
    }
    Ok(())
}

pub fn escape_graphql_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                use std::fmt::Write;
                write!(escaped, "\\u{:04x}", ch as u32).expect("writing to a String");
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

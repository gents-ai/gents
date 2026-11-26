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

pub const DEFRADB_FILTER_OPERATORS_HELP: &str =
    "_eq, _neq, _gt, _geq, _lt, _leq, _in, _nin, _like, _nlike, _ilike, _nilike; compose with _and, _or, _not";

pub const DEFRADB_FILTER_OPERATORS: &[&str] = &[
    "_eq",
    "_neq",
    "_ne",
    "_gt",
    "_geq",
    "_gte",
    "_ge",
    "_lt",
    "_leq",
    "_lte",
    "_le",
    "_in",
    "_nin",
    "_like",
    "_nlike",
    "_ilike",
    "_nilike",
    "_contains",
    "_contained_in",
    "_has_key",
    "_and",
    "_or",
    "_not",
    "_any",
    "_all",
    "_none",
];

fn is_value_filter_operator(name: &str) -> bool {
    matches!(
        name,
        "_eq"
            | "_neq"
            | "_ne"
            | "_gt"
            | "_geq"
            | "_gte"
            | "_ge"
            | "_lt"
            | "_leq"
            | "_lte"
            | "_le"
            | "_in"
            | "_nin"
            | "_like"
            | "_nlike"
            | "_ilike"
            | "_nilike"
            | "_contains"
            | "_contained_in"
            | "_has_key"
    )
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
    graphql_filter_top_level_fields(filter).map(|_| ())
}

pub fn graphql_filter_top_level_fields(filter: &str) -> Result<Vec<String>> {
    let trimmed = scan_filter_tokens(filter)?;
    let mut parser = FilterParser {
        bytes: trimmed.as_bytes(),
        pos: 0,
    };
    let mut fields = Vec::new();
    parser.parse_filter_object(&mut fields, true)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(GraphqlError::FilterTrailingContent);
    }
    Ok(fields)
}

fn scan_filter_tokens(filter: &str) -> Result<&str> {
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
    Ok(trimmed)
}

struct FilterParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> FilterParser<'a> {
    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    fn skip_ws_and_commas(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b',')
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect_byte(&mut self, expected: u8, message: &'static str) -> Result<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(GraphqlError::FilterSyntax { message })
        }
    }

    fn read_name(&mut self) -> Result<&'a str> {
        self.skip_ws();
        let start = self.pos;
        match self.bytes.get(self.pos) {
            Some(b) if b.is_ascii_alphabetic() || *b == b'_' => self.pos += 1,
            _ => {
                return Err(GraphqlError::FilterSyntax {
                    message: "expected a GraphQL field or operator name",
                });
            }
        }
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.pos += 1;
        }
        Ok(std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII identifier"))
    }

    fn parse_filter_object(&mut self, fields: &mut Vec<String>, collect: bool) -> Result<()> {
        self.expect_byte(b'{', "expected '{' to start filter object")?;
        loop {
            self.skip_ws_and_commas();
            if self.bytes.get(self.pos) == Some(&b'}') {
                self.pos += 1;
                return Ok(());
            }
            let key = self.read_name()?;
            self.expect_byte(b':', "expected ':' after filter key")?;
            match key {
                "_and" | "_or" => self.parse_filter_object_or_list(fields, collect)?,
                "_not" => self.parse_filter_object(fields, collect)?,
                op if op.starts_with('_') && op != "_docID" => {
                    return Err(GraphqlError::FilterUnknownOperator {
                        operator: op.to_owned(),
                    });
                }
                field => {
                    if collect {
                        fields.push(field.to_owned());
                    }
                    self.parse_field_condition()?;
                }
            }
        }
    }

    fn parse_filter_object_or_list(
        &mut self,
        fields: &mut Vec<String>,
        collect: bool,
    ) -> Result<()> {
        if self.peek() == Some(b'[') {
            self.pos += 1;
            loop {
                self.skip_ws_and_commas();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(());
                }
                self.parse_filter_object(fields, collect)?;
            }
        }
        self.parse_filter_object(fields, collect)
    }

    fn parse_field_condition(&mut self) -> Result<()> {
        if self.peek() == Some(b'{') {
            self.parse_condition_object()
        } else {
            self.skip_value()
        }
    }

    fn parse_condition_object(&mut self) -> Result<()> {
        self.expect_byte(b'{', "expected '{' to start filter condition")?;
        loop {
            self.skip_ws_and_commas();
            if self.bytes.get(self.pos) == Some(&b'}') {
                self.pos += 1;
                return Ok(());
            }
            let key = self.read_name()?;
            self.expect_byte(b':', "expected ':' after filter operator")?;
            match key {
                "_and" | "_or" => self.parse_condition_object_or_list()?,
                "_not" | "_any" | "_all" | "_none" => self.parse_field_condition()?,
                op if is_value_filter_operator(op) => self.skip_value()?,
                op if op.starts_with('_') && op != "_docID" => {
                    return Err(GraphqlError::FilterUnknownOperator {
                        operator: op.to_owned(),
                    });
                }
                _ => self.parse_field_condition()?,
            }
        }
    }

    fn parse_condition_object_or_list(&mut self) -> Result<()> {
        if self.peek() == Some(b'[') {
            self.pos += 1;
            loop {
                self.skip_ws_and_commas();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(());
                }
                self.parse_condition_object()?;
            }
        }
        self.parse_condition_object()
    }

    fn skip_value(&mut self) -> Result<()> {
        match self.peek() {
            Some(b'"') => {
                self.skip_string();
                Ok(())
            }
            Some(b'{') => self.skip_object(),
            Some(b'[') => self.skip_list(),
            Some(b) if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.') => {
                while self.bytes.get(self.pos).is_some_and(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.')
                }) {
                    self.pos += 1;
                }
                Ok(())
            }
            _ => Err(GraphqlError::FilterSyntax {
                message: "expected a filter value",
            }),
        }
    }

    fn skip_string(&mut self) {
        self.pos += 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            self.pos += 1;
            if b == b'\\' {
                self.pos += 1;
            } else if b == b'"' {
                break;
            }
        }
    }

    fn skip_object(&mut self) -> Result<()> {
        self.pos += 1;
        loop {
            self.skip_ws_and_commas();
            if self.bytes.get(self.pos) == Some(&b'}') {
                self.pos += 1;
                return Ok(());
            }
            let _ = self.read_name()?;
            self.expect_byte(b':', "expected ':' in object literal")?;
            self.skip_value()?;
        }
    }

    fn skip_list(&mut self) -> Result<()> {
        self.pos += 1;
        loop {
            self.skip_ws_and_commas();
            if self.bytes.get(self.pos) == Some(&b']') {
                self.pos += 1;
                return Ok(());
            }
            self.skip_value()?;
        }
    }
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

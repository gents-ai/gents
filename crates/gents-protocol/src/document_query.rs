//! Typed GraphQL documents over the generated schema fields.
//!
//! Every value travels as a GraphQL variable, so callers never escape or
//! interpolate, and a field can only be compared with or assigned a value of
//! its declared kind. Rendering is pure; the store layer executes the result.

use std::fmt::Write as _;
use std::marker::PhantomData;

pub use gents_schemas::{fields, kind, Collection, Field};

use gents_schemas::kind::Kind;
use serde_json::{Map, Value};

/// A rendered operation: the document text and the variables it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub document: String,
    pub variables: Value,
}

/// A Rust value accepted where a field of kind `K` is compared or assigned.
pub trait Scalar<K> {
    fn into_json(self) -> Value;
}

macro_rules! string_scalar {
    ($($kind:ident),*) => {
        $(
            impl Scalar<kind::$kind> for &str {
                fn into_json(self) -> Value {
                    Value::String(self.to_owned())
                }
            }
            impl Scalar<kind::$kind> for &String {
                fn into_json(self) -> Value {
                    Value::String(self.clone())
                }
            }
            impl Scalar<kind::$kind> for String {
                fn into_json(self) -> Value {
                    Value::String(self)
                }
            }
        )*
    };
}

string_scalar!(Str, DateTime, DocId);

macro_rules! integer_scalar {
    ($($ty:ty),*) => {
        $(
            impl Scalar<kind::Int> for $ty {
                fn into_json(self) -> Value {
                    Value::from(self)
                }
            }
        )*
    };
}

integer_scalar!(i32, i64, u32, u64, usize);

impl Scalar<kind::Bool> for bool {
    fn into_json(self) -> Value {
        Value::Bool(self)
    }
}

impl Scalar<kind::Float> for f64 {
    fn into_json(self) -> Value {
        Value::from(self)
    }
}

impl Scalar<kind::Json> for Value {
    fn into_json(self) -> Value {
        self
    }
}

impl Scalar<kind::Json> for &Value {
    fn into_json(self) -> Value {
        self.clone()
    }
}

/// Kinds with a total order DefraDB can range over.
pub trait Ordered {}
impl Ordered for kind::Str {}
impl Ordered for kind::DocId {}
impl Ordered for kind::Int {}
impl Ordered for kind::Float {}
impl Ordered for kind::DateTime {}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Compare {
        field: &'static str,
        operator: &'static str,
        variable_type: String,
        value: Value,
    },
    Null {
        field: &'static str,
        operator: &'static str,
    },
    Contains {
        field: &'static str,
        value: Value,
    },
    Nested {
        field: &'static str,
        inner: Box<Expr>,
    },
    Group {
        operator: &'static str,
        members: Vec<Expr>,
    },
    Not(Box<Expr>),
}

pub struct JsonPath<C, K> {
    field: Field<C, kind::Json>,
    path: &'static [&'static str],
    kind: PhantomData<fn() -> K>,
}

impl<C, K> JsonPath<C, K> {
    pub const fn new(field: Field<C, kind::Json>, path: &'static [&'static str]) -> Self {
        assert!(!path.is_empty(), "JSON path must have a leaf");
        Self {
            field,
            path,
            kind: PhantomData,
        }
    }
}

impl<C, K> Clone for JsonPath<C, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C, K> Copy for JsonPath<C, K> {}

/// A filter over collection `C`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter<C> {
    expr: Expr,
    collection: PhantomData<fn() -> C>,
}

impl<C> Filter<C> {
    fn wrap(expr: Expr) -> Self {
        Self {
            expr,
            collection: PhantomData,
        }
    }

    fn compare<K, V>(field: Field<C, K>, operator: &'static str, value: V) -> Self
    where
        K: Kind,
        V: Scalar<K>,
    {
        Self::wrap(Expr::Compare {
            field: field.name(),
            operator,
            variable_type: K::GRAPHQL.to_owned(),
            value: value.into_json(),
        })
    }

    pub fn eq<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind,
        V: Scalar<K>,
    {
        Self::compare(field, "_eq", value)
    }

    fn json_compare<K, V>(path: JsonPath<C, K>, operator: &'static str, value: V) -> Self
    where
        K: Kind,
        V: Scalar<K>,
    {
        for name in path.path {
            crate::graphql::validate_graphql_name(name)
                .expect("JSON path keys are declared GraphQL names");
        }
        let (leaf, parents) = path.path.split_last().expect("JSON path has a leaf");
        let mut expr = Expr::Compare {
            field: leaf,
            operator,
            variable_type: K::GRAPHQL.to_owned(),
            value: value.into_json(),
        };
        for field in parents.iter().rev() {
            expr = Expr::Nested {
                field,
                inner: Box::new(expr),
            };
        }
        Self::wrap(Expr::Nested {
            field: path.field.name(),
            inner: Box::new(expr),
        })
    }

    pub fn json_eq<K, V>(path: JsonPath<C, K>, value: V) -> Self
    where
        K: Kind,
        V: Scalar<K>,
    {
        Self::json_compare(path, "_eq", value)
    }

    pub fn json_like<V>(path: JsonPath<C, kind::Str>, pattern: V) -> Self
    where
        V: Scalar<kind::Str>,
    {
        Self::json_compare(path, "_like", pattern)
    }

    pub fn ne<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind,
        V: Scalar<K>,
    {
        Self::compare(field, "_ne", value)
    }

    pub fn gt<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind + Ordered,
        V: Scalar<K>,
    {
        Self::compare(field, "_gt", value)
    }

    pub fn ge<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind + Ordered,
        V: Scalar<K>,
    {
        Self::compare(field, "_ge", value)
    }

    pub fn lt<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind + Ordered,
        V: Scalar<K>,
    {
        Self::compare(field, "_lt", value)
    }

    pub fn le<K, V>(field: Field<C, K>, value: V) -> Self
    where
        K: Kind + Ordered,
        V: Scalar<K>,
    {
        Self::compare(field, "_le", value)
    }

    pub fn like<V>(field: Field<C, kind::Str>, pattern: V) -> Self
    where
        V: Scalar<kind::Str>,
    {
        Self::compare(field, "_like", pattern)
    }

    pub fn is_null<K>(field: Field<C, K>) -> Self {
        Self::wrap(Expr::Null {
            field: field.name(),
            operator: "_eq",
        })
    }

    pub fn not_null<K>(field: Field<C, K>) -> Self {
        Self::wrap(Expr::Null {
            field: field.name(),
            operator: "_ne",
        })
    }

    pub fn contains<V>(field: Field<C, kind::StrList>, value: V) -> Self
    where
        V: Scalar<kind::Str>,
    {
        Self::wrap(Expr::Contains {
            field: field.name(),
            value: value.into_json(),
        })
    }

    /// Matches when the field equals any of `values`. An empty set matches no
    /// document, which DefraDB evaluates the same way.
    pub fn any_of<K, V, I>(field: Field<C, K>, values: I) -> Self
    where
        K: Kind,
        V: Scalar<K>,
        I: IntoIterator<Item = V>,
    {
        Self::wrap(Expr::Compare {
            field: field.name(),
            operator: "_in",
            variable_type: format!("[{}!]", K::GRAPHQL),
            value: Value::Array(values.into_iter().map(Scalar::into_json).collect()),
        })
    }

    pub fn none_of<K, V, I>(field: Field<C, K>, values: I) -> Self
    where
        K: Kind,
        V: Scalar<K>,
        I: IntoIterator<Item = V>,
    {
        Self::wrap(Expr::Compare {
            field: field.name(),
            operator: "_nin",
            variable_type: format!("[{}!]", K::GRAPHQL),
            value: Value::Array(values.into_iter().map(Scalar::into_json).collect()),
        })
    }

    pub fn all<I>(filters: I) -> Self
    where
        I: IntoIterator<Item = Filter<C>>,
    {
        Self::wrap(Expr::Group {
            operator: "_and",
            members: filters.into_iter().map(|filter| filter.expr).collect(),
        })
    }

    pub fn any<I>(filters: I) -> Self
    where
        I: IntoIterator<Item = Filter<C>>,
    {
        Self::wrap(Expr::Group {
            operator: "_or",
            members: filters.into_iter().map(|filter| filter.expr).collect(),
        })
    }

    pub fn and(self, other: Filter<C>) -> Self {
        let mut members = match self.expr {
            Expr::Group {
                operator: "_and",
                members,
            } => members,
            expr => vec![expr],
        };
        members.push(other.expr);
        Self::wrap(Expr::Group {
            operator: "_and",
            members,
        })
    }

    pub fn negate(self) -> Self {
        Self::wrap(Expr::Not(Box::new(self.expr)))
    }
}

/// The filter every session-owned collection shares: its node, session and
/// requester. A null requester matches documents written without one.
pub fn session_scope<C>(node_did: &str, session_id: &str, requester_did: Option<&str>) -> Filter<C>
where
    C: gents_schemas::SessionScoped,
{
    let requester = match requester_did {
        Some(did) => Filter::eq(C::REQUESTER_DID, did),
        None => Filter::is_null(C::REQUESTER_DID),
    };
    Filter::eq(C::NODE_DID, node_did)
        .and(Filter::eq(C::SESSION_ID, session_id))
        .and(requester)
}

/// Variables accumulated while rendering one document.
#[derive(Debug, Default)]
struct Variables {
    declarations: Vec<String>,
    values: Map<String, Value>,
}

impl Variables {
    fn bind(&mut self, variable_type: &str, value: Value) -> String {
        let name = format!("v{}", self.declarations.len());
        self.declarations.push(format!("${name}: {variable_type}"));
        self.values.insert(name.clone(), value);
        format!("${name}")
    }

    fn header(&self) -> String {
        if self.declarations.is_empty() {
            String::new()
        } else {
            format!("({})", self.declarations.join(", "))
        }
    }
}

fn render_expr(expr: &Expr, variables: &mut Variables, out: &mut String) {
    match expr {
        Expr::Compare {
            field,
            operator,
            variable_type,
            value,
        } => {
            let reference = variables.bind(variable_type, value.clone());
            write!(out, "{field}: {{{operator}: {reference}}}").expect("write to string");
        }
        Expr::Null { field, operator } => {
            write!(out, "{field}: {{{operator}: null}}").expect("write to string");
        }
        Expr::Contains { field, value } => {
            let reference = variables.bind("String", value.clone());
            write!(out, "{field}: {{_any: {{_eq: {reference}}}}}").expect("write to string");
        }
        Expr::Nested { field, inner } => {
            write!(out, "{field}: {{").expect("write to string");
            render_expr(inner, variables, out);
            out.push('}');
        }
        Expr::Group { operator, members } if *operator == "_and" && distinct_leaves(members) => {
            for (index, member) in members.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                render_expr(member, variables, out);
            }
        }
        Expr::Group { operator, members } => {
            write!(out, "{operator}: [").expect("write to string");
            for (index, member) in members.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push('{');
                render_expr(member, variables, out);
                out.push('}');
            }
            out.push(']');
        }
        Expr::Not(inner) => {
            out.push_str("_not: {");
            render_expr(inner, variables, out);
            out.push('}');
        }
    }
}

/// A conjunction of comparisons on distinct fields renders as sibling keys,
/// the form DefraDB plans against a composite index; `_and` is kept for a
/// field that appears twice and for nested groups.
fn distinct_leaves(members: &[Expr]) -> bool {
    let mut fields = Vec::with_capacity(members.len());
    for member in members {
        let field = match member {
            Expr::Compare { field, .. }
            | Expr::Null { field, .. }
            | Expr::Contains { field, .. }
            | Expr::Nested { field, .. } => *field,
            Expr::Group { .. } | Expr::Not(_) => return false,
        };
        if fields.contains(&field) {
            return false;
        }
        fields.push(field);
    }
    !members.is_empty()
}

fn render_filter(expr: &Expr, variables: &mut Variables) -> String {
    let mut out = String::from("{");
    render_expr(expr, variables, &mut out);
    out.push('}');
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// The fields an operation returns. Field lists come from generated constants
/// or from a selection constant an owner already publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<C> {
    body: String,
    collection: PhantomData<fn() -> C>,
}

impl<C> Selection<C> {
    pub fn all() -> Self
    where
        C: gents_schemas::SelectFields,
    {
        C::select_fields(Self::new())
    }

    pub fn doc_id() -> Self {
        Self::owned("_docID")
    }

    /// A selection text an owner publishes as a constant, such as
    /// `AGENT_REQUEST_FIELDS`. It is never built from runtime values.
    pub fn owned(body: &'static str) -> Self {
        Self {
            body: body.trim().to_owned(),
            collection: PhantomData,
        }
    }

    pub fn field<K>(mut self, field: Field<C, K>) -> Self {
        if !self.body.is_empty() {
            self.body.push(' ');
        }
        self.body.push_str(field.name());
        self
    }

    pub fn nested<T>(mut self, field: Field<C, kind::Relation<T>>, selection: Selection<T>) -> Self
    where
        T: Collection,
    {
        if !self.body.is_empty() {
            self.body.push(' ');
        }
        write!(self.body, "{} {{ {} }}", field.name(), selection.body).expect("write to string");
        self
    }

    pub fn new() -> Self {
        Self {
            body: String::new(),
            collection: PhantomData,
        }
    }
}

impl<C> Default for Selection<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> gents_schemas::FieldSelection<C> for Selection<C> {
    fn field<K>(self, field: Field<C, K>) -> Self {
        Selection::field(self, field)
    }
}

/// A read of collection `C`.
#[derive(Debug, Clone, PartialEq)]
pub struct Query<C> {
    filter: Option<Expr>,
    doc_id: Option<String>,
    order: Vec<(&'static str, Direction)>,
    limit: Option<u64>,
    offset: Option<u64>,
    selection: Selection<C>,
    count: bool,
}

impl<C> Query<C>
where
    C: Collection,
{
    pub fn select(selection: Selection<C>) -> Self {
        Self {
            filter: None,
            doc_id: None,
            order: Vec::new(),
            limit: None,
            offset: None,
            selection,
            count: false,
        }
    }

    pub fn count() -> Self {
        Self::select(Selection::new()).counted()
    }

    pub fn select_fields(mut self, selection: Selection<C>) -> Self {
        self.selection = selection;
        self.count = false;
        self
    }

    pub fn counted(mut self) -> Self {
        self.order.clear();
        self.limit = None;
        self.offset = None;
        self.selection = Selection::new();
        self.count = true;
        self
    }

    pub fn is_count(&self) -> bool {
        self.count
    }

    pub fn filter(mut self, filter: Filter<C>) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => Filter::<C>::wrap(existing).and(filter).expr,
            None => filter.expr,
        });
        self
    }

    pub fn doc_id(mut self, doc_id: impl Into<String>) -> Self {
        self.doc_id = Some(doc_id.into());
        self
    }

    pub fn order_by<K>(mut self, field: Field<C, K>, direction: Direction) -> Self {
        self.order.push((field.name(), direction));
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    fn render_root(&self, alias: Option<&str>, variables: &mut Variables) -> String {
        let mut arguments = Vec::new();
        if let Some(doc_id) = self.doc_id.as_ref().filter(|_| !self.count) {
            let reference = variables.bind("ID!", Value::String(doc_id.clone()));
            arguments.push(format!("docID: {reference}"));
        }
        let count_filter = self.doc_id.as_ref().filter(|_| self.count).map(|doc_id| {
            let doc_filter = Filter::eq(Field::<C, kind::DocId>::new("_docID"), doc_id);
            match &self.filter {
                Some(filter) => Filter::<C>::wrap(filter.clone()).and(doc_filter),
                None => doc_filter,
            }
        });
        let filter = count_filter
            .as_ref()
            .map(|filter| &filter.expr)
            .or(self.filter.as_ref());
        if let Some(filter) = filter {
            arguments.push(format!("filter: {}", render_filter(filter, variables)));
        }
        if !self.order.is_empty() {
            let order: Vec<_> = self
                .order
                .iter()
                .map(|(field, direction)| format!("{field}: {}", direction.as_str()))
                .collect();
            arguments.push(format!("order: {{{}}}", order.join(", ")));
        }
        if let Some(limit) = self.limit {
            let reference = variables.bind("Int", Value::from(limit));
            arguments.push(format!("limit: {reference}"));
        }
        if let Some(offset) = self.offset {
            let reference = variables.bind("Int", Value::from(offset));
            arguments.push(format!("offset: {reference}"));
        }
        if self.count {
            return format!(
                "{}: COUNT({}: {{{}}})",
                alias.unwrap_or(C::NAME),
                C::NAME,
                arguments.join(", ")
            );
        }
        let arguments = if arguments.is_empty() {
            String::new()
        } else {
            format!("({})", arguments.join(", "))
        };
        let alias = alias.map(|alias| format!("{alias}: ")).unwrap_or_default();
        format!(
            "{alias}{}{arguments} {{ {} }}",
            C::NAME,
            self.selection.body
        )
    }

    pub fn render(&self) -> Rendered {
        let mut variables = Variables::default();
        let root = self.render_root(None, &mut variables);
        Rendered {
            document: format!("query{} {{ {root} }}", variables.header()),
            variables: Value::Object(variables.values),
        }
    }
}

/// Several reads in one document, each under its own alias, so they observe
/// one snapshot.
#[derive(Debug, Default)]
pub struct QuerySet {
    roots: Vec<String>,
    variables: Variables,
}

impl QuerySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn root<C>(mut self, alias: &'static str, query: &Query<C>) -> Self
    where
        C: Collection,
    {
        let root = query.render_root(Some(alias), &mut self.variables);
        self.roots.push(root);
        self
    }

    pub fn indexed<C>(mut self, index: usize, query: &Query<C>) -> Self
    where
        C: Collection,
    {
        let alias = format!("view_{index}");
        let root = query.render_root(Some(&alias), &mut self.variables);
        self.roots.push(root);
        self
    }

    /// A root answered under its collection name.
    pub fn plain<C>(mut self, query: &Query<C>) -> Self
    where
        C: Collection,
    {
        let root = query.render_root(None, &mut self.variables);
        self.roots.push(root);
        self
    }

    pub fn render(self) -> Rendered {
        Rendered {
            document: format!(
                "query{} {{ {} }}",
                self.variables.header(),
                self.roots.join(" ")
            ),
            variables: Value::Object(self.variables.values),
        }
    }
}

/// A Rust value assignable to a field of kind `K`.
pub trait Assign<K> {
    fn into_json(self) -> Value;
}

impl<K, V> Assign<K> for V
where
    V: Scalar<K>,
{
    fn into_json(self) -> Value {
        Scalar::into_json(self)
    }
}

/// The fields a create or update writes to collection `C`.
#[derive(Debug, Clone, PartialEq)]
pub struct Input<C> {
    fields: Map<String, Value>,
    collection: PhantomData<fn() -> C>,
}

impl<C> Default for Input<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> Input<C> {
    pub fn new() -> Self {
        Self {
            fields: Map::new(),
            collection: PhantomData,
        }
    }

    pub fn set<K, V>(mut self, field: Field<C, K>, value: V) -> Self
    where
        V: Assign<K>,
    {
        self.fields
            .insert(field.name().to_owned(), value.into_json());
        self
    }

    /// Assigns the value when present and writes an explicit null otherwise.
    pub fn set_optional<K, V>(mut self, field: Field<C, K>, value: Option<V>) -> Self
    where
        V: Assign<K>,
    {
        self.fields.insert(
            field.name().to_owned(),
            value.map(Assign::into_json).unwrap_or(Value::Null),
        );
        self
    }

    pub fn set_null<K>(mut self, field: Field<C, K>) -> Self {
        self.fields.insert(field.name().to_owned(), Value::Null);
        self
    }

    /// DefraDB rejects an empty list literal for a nillable list field, so an
    /// empty list is written as null.
    pub fn set_list<V, I>(mut self, field: Field<C, kind::StrList>, values: I) -> Self
    where
        V: Into<String>,
        I: IntoIterator<Item = V>,
    {
        let values: Vec<Value> = values
            .into_iter()
            .map(|value| Value::String(value.into()))
            .collect();
        let value = if values.is_empty() {
            Value::Null
        } else {
            Value::Array(values)
        };
        self.fields.insert(field.name().to_owned(), value);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// An input a canonical row type already serializes field for field. The
    /// row type owns the names; a serialized value that is not an object
    /// writes nothing.
    pub fn from_serialized(row: Value) -> Self {
        Self {
            fields: match row {
                Value::Object(fields) => fields,
                _ => Map::new(),
            },
            collection: PhantomData,
        }
    }
}

fn input_type<C>() -> String
where
    C: Collection,
{
    format!("{}MutationInputArg!", C::NAME)
}

/// Creates one document in collection `C`.
#[derive(Debug, Clone, PartialEq)]
pub struct Create<C> {
    input: Input<C>,
    selection: Selection<C>,
}

impl<C> Create<C>
where
    C: Collection,
{
    pub fn new(input: Input<C>) -> Self {
        Self {
            input,
            selection: Selection::doc_id(),
        }
    }

    pub fn returning(mut self, selection: Selection<C>) -> Self {
        self.selection = selection;
        self
    }

    pub fn render(&self) -> Rendered {
        let mut variables = Variables::default();
        let input = variables.bind(&input_type::<C>(), Value::Object(self.input.fields.clone()));
        Rendered {
            document: format!(
                "mutation{} {{ create_{}(input: {input}) {{ {} }} }}",
                variables.header(),
                C::NAME,
                self.selection.body
            ),
            variables: Value::Object(variables.values),
        }
    }
}

/// Updates the documents of collection `C` that the document ID and filter
/// select. The filter is the compare half of a compare-and-set: an update that
/// matches nothing returns no rows and writes nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Update<C> {
    doc_id: Option<String>,
    filter: Option<Expr>,
    input: Input<C>,
    selection: Selection<C>,
}

impl<C> Update<C>
where
    C: Collection,
{
    pub fn doc(doc_id: impl Into<String>, input: Input<C>) -> Self {
        Self {
            doc_id: Some(doc_id.into()),
            filter: None,
            input,
            selection: Selection::doc_id(),
        }
    }

    pub fn matching(filter: Filter<C>, input: Input<C>) -> Self {
        Self {
            doc_id: None,
            filter: Some(filter.expr),
            input,
            selection: Selection::doc_id(),
        }
    }

    pub fn filter(mut self, filter: Filter<C>) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => Filter::<C>::wrap(existing).and(filter).expr,
            None => filter.expr,
        });
        self
    }

    pub fn returning(mut self, selection: Selection<C>) -> Self {
        self.selection = selection;
        self
    }

    pub fn render(&self) -> Rendered {
        let mut variables = Variables::default();
        let mut arguments = Vec::new();
        if let Some(doc_id) = &self.doc_id {
            let reference = variables.bind("ID!", Value::String(doc_id.clone()));
            arguments.push(format!("docID: {reference}"));
        }
        if let Some(filter) = &self.filter {
            arguments.push(format!("filter: {}", render_filter(filter, &mut variables)));
        }
        let input = variables.bind(&input_type::<C>(), Value::Object(self.input.fields.clone()));
        arguments.push(format!("input: {input}"));
        Rendered {
            document: format!(
                "mutation{} {{ update_{}({}) {{ {} }} }}",
                variables.header(),
                C::NAME,
                arguments.join(", "),
                self.selection.body
            ),
            variables: Value::Object(variables.values),
        }
    }
}

/// Deletes the documents of collection `C` that the document ID and filter
/// select.
#[derive(Debug, Clone, PartialEq)]
pub struct Delete<C> {
    doc_id: Option<String>,
    filter: Option<Expr>,
    collection: PhantomData<fn() -> C>,
}

impl<C> Delete<C>
where
    C: Collection,
{
    pub fn doc(doc_id: impl Into<String>) -> Self {
        Self {
            doc_id: Some(doc_id.into()),
            filter: None,
            collection: PhantomData,
        }
    }

    pub fn matching(filter: Filter<C>) -> Self {
        Self {
            doc_id: None,
            filter: Some(filter.expr),
            collection: PhantomData,
        }
    }

    pub fn filter(mut self, filter: Filter<C>) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => Filter::<C>::wrap(existing).and(filter).expr,
            None => filter.expr,
        });
        self
    }

    pub fn render(&self) -> Rendered {
        let mut variables = Variables::default();
        let mut arguments = Vec::new();
        if let Some(doc_id) = &self.doc_id {
            let reference = variables.bind("ID!", Value::String(doc_id.clone()));
            arguments.push(format!("docID: {reference}"));
        }
        if let Some(filter) = &self.filter {
            arguments.push(format!("filter: {}", render_filter(filter, &mut variables)));
        }
        Rendered {
            document: format!(
                "mutation{} {{ delete_{}({}) {{ _docID }} }}",
                variables.header(),
                C::NAME,
                arguments.join(", ")
            ),
            variables: Value::Object(variables.values),
        }
    }
}

/// Typed reads of one returned row.
pub trait Decode {
    type Output<'a>;
    fn decode(value: &Value) -> Option<Self::Output<'_>>;
}

macro_rules! decode_str {
    ($($kind:ident),*) => {
        $(
            impl Decode for kind::$kind {
                type Output<'a> = &'a str;
                fn decode(value: &Value) -> Option<&str> {
                    value.as_str()
                }
            }
        )*
    };
}

decode_str!(Str, DateTime, DocId);

impl Decode for kind::Int {
    type Output<'a> = i64;
    fn decode(value: &Value) -> Option<i64> {
        value.as_i64()
    }
}

impl Decode for kind::Bool {
    type Output<'a> = bool;
    fn decode(value: &Value) -> Option<bool> {
        value.as_bool()
    }
}

impl Decode for kind::Float {
    type Output<'a> = f64;
    fn decode(value: &Value) -> Option<f64> {
        value.as_f64()
    }
}

impl Decode for kind::Json {
    type Output<'a> = &'a Value;
    fn decode(value: &Value) -> Option<&Value> {
        (!value.is_null()).then_some(value)
    }
}

impl Decode for kind::StrList {
    type Output<'a> = Vec<&'a str>;
    fn decode(value: &Value) -> Option<Vec<&str>> {
        value.as_array()?.iter().map(Value::as_str).collect()
    }
}

/// Reads `field` from a row an operation on collection `C` returned. Absent,
/// null and wrongly typed values all read as `None`.
pub fn get<'a, C, K>(row: &'a Value, field: Field<C, K>) -> Option<K::Output<'a>>
where
    K: Decode,
{
    K::decode(row.get(field.name())?)
}

/// The rows a read returned under `root`: the collection name, or an alias
/// from a [`QuerySet`]. Mutation rows go through the store's mutation
/// response owner, because DefraDB answers a `create_` under `add_`.
pub fn rows<'a>(response: &'a Value, root: &str) -> &'a [Value] {
    response
        .get("data")
        .unwrap_or(response)
        .get(root)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_schemas::fields::agent_request as request;
    use serde_json::json;

    #[test]
    fn query_binds_every_value_as_a_variable() {
        let hostile = "x\" }) { _docID } #";
        let rendered = Query::select(Selection::new().field(request::REQUEST_ID))
            .filter(Filter::eq(request::NODE_DID, hostile))
            .filter(Filter::any_of(
                request::LIFECYCLE_STATE,
                ["pending", "claimed"],
            ))
            .filter(Filter::is_null(request::TERMINALIZED_AT))
            .order_by(request::CREATED_AT, Direction::Desc)
            .limit(5)
            .render();
        assert_eq!(
            rendered.document,
            "query($v0: String, $v1: [String!], $v2: Int) { AgentRequest(filter: {node_did: {_eq: $v0}, lifecycle_state: {_in: $v1}, terminalized_at: {_eq: null}}, order: {created_at: DESC}, limit: $v2) { request_id } }"
        );
        assert_eq!(
            rendered.variables,
            json!({"v0": hostile, "v1": ["pending", "claimed"], "v2": 5})
        );
        assert!(!rendered.document.contains(hostile));
    }

    #[test]
    fn a_repeated_field_keeps_an_explicit_conjunction() {
        let rendered = Query::select(Selection::doc_id())
            .filter(Filter::ge(request::CREATED_AT, "a"))
            .filter(Filter::lt(request::CREATED_AT, "b"))
            .render();
        assert_eq!(
            rendered.document,
            "query($v0: String, $v1: String) { AgentRequest(filter: {_and: [{created_at: {_ge: $v0}}, {created_at: {_lt: $v1}}]}) { _docID } }"
        );
    }

    #[test]
    fn session_scope_matches_the_string_fragment_it_replaces() {
        use gents_schemas::fields::agent_message as message;
        let scoped = Query::select(Selection::new().field(message::SEQUENCE))
            .filter(session_scope("did:node", "s1", None))
            .render();
        assert_eq!(
            scoped.document,
            "query($v0: String, $v1: String) { AgentMessage(filter: {node_did: {_eq: $v0}, session_id: {_eq: $v1}, requester_did: {_eq: null}}) { sequence } }"
        );
        let requester = Query::<message::Doc>::select(Selection::doc_id())
            .filter(session_scope("did:node", "s1", Some("did:user")))
            .render();
        assert_eq!(
            requester.variables,
            json!({"v0": "did:node", "v1": "s1", "v2": "did:user"})
        );
    }

    #[test]
    fn update_is_a_compare_and_set_over_variables() {
        let rendered = Update::doc(
            "bae-1",
            Input::new()
                .set(request::LIFECYCLE_STATE, "failed")
                .set(request::TERMINAL_REDRIVE_ATTEMPTS, 0)
                .set(request::TERMINAL_OUTPUT, json!({"kind": "no_message"}))
                .set_null(request::CLAIMED_AT),
        )
        .filter(Filter::eq(request::LIFECYCLE_STATE, "pending"))
        .returning(Selection::doc_id().field(request::REQUEST_ID))
        .render();
        assert_eq!(
            rendered.document,
            "mutation($v0: ID!, $v1: String, $v2: AgentRequestMutationInputArg!) { update_AgentRequest(docID: $v0, filter: {lifecycle_state: {_eq: $v1}}, input: $v2) { _docID request_id } }"
        );
        assert_eq!(
            rendered.variables,
            json!({
                "v0": "bae-1",
                "v1": "pending",
                "v2": {
                    "lifecycle_state": "failed",
                    "terminal_redrive_attempts": 0,
                    "terminal_output": {"kind": "no_message"},
                    "claimed_at": null
                }
            })
        );
    }

    #[test]
    fn query_set_shares_one_variable_namespace() {
        let newest = Query::select(Selection::new().field(request::CREATED_AT))
            .filter(Filter::eq(request::SESSION_ID, "s"))
            .limit(1);
        let pending = Query::select(Selection::doc_id())
            .filter(Filter::eq(request::LIFECYCLE_STATE, "pending"));
        let rendered = QuerySet::new()
            .root("newest", &newest)
            .root("pending", &pending)
            .render();
        assert_eq!(
            rendered.document,
            "query($v0: String, $v1: Int, $v2: String) { newest: AgentRequest(filter: {session_id: {_eq: $v0}}, limit: $v1) { created_at } pending: AgentRequest(filter: {lifecycle_state: {_eq: $v2}}) { _docID } }"
        );
        assert_eq!(
            rendered.variables,
            json!({"v0": "s", "v1": 1, "v2": "pending"})
        );
    }

    #[test]
    fn rows_and_fields_decode_by_kind() {
        let response = json!({"data": {"AgentRequest": [
            {"request_id": "r1", "request_hop": 2, "relationships_pending": true, "input": {"a": 1}}
        ]}});
        let row = &rows(&response, "AgentRequest")[0];
        assert_eq!(get(row, request::REQUEST_ID), Some("r1"));
        assert_eq!(get(row, request::REQUEST_HOP), Some(2));
        assert_eq!(get(row, request::RELATIONSHIPS_PENDING), Some(true));
        assert_eq!(get(row, request::INPUT), Some(&json!({"a": 1})));
        assert_eq!(get(row, request::CLAIMED_AT), None);
        assert!(rows(&response, "Missing").is_empty());
    }

    #[test]
    fn empty_list_input_is_written_as_null() {
        use gents_schemas::fields::tools;
        let empty = Create::new(Input::new().set_list(tools::TAGS, Vec::<String>::new())).render();
        assert_eq!(empty.variables, json!({"v0": {"tags": null}}));
        let filled = Create::new(Input::new().set_list(tools::TAGS, ["a"])).render();
        assert_eq!(filled.variables, json!({"v0": {"tags": ["a"]}}));
        assert_eq!(
            filled.document,
            "mutation($v0: ToolsMutationInputArg!) { create_Tools(input: $v0) { _docID } }"
        );
    }
}

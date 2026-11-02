pub(super) fn page(path: &[&str]) -> Option<&'static str> {
    Some(match path {
        ["fields"] => "Fields declare document values in GraphQL SDL. Read help fields TOPIC: types, nullability, arrays, defaults, crdt, immutable, constraints. Collection-valued fields are relationships; see help relationships. To publish SDL, read collection create --help.",
        ["fields", "types"] => r#"Scalar types: String, Boolean, Int, Float (Float64), Float32, Float64, DateTime, JSON, Blob and ID. Use Boolean in SDL, not Bool. DefraDB supplies _docID; a business identifier can be a String.
```graphql
type Reading {
  label: String
  active: Boolean
  count: Int
  measuredAt: DateTime
}
```
A collection name used as a field type declares a relationship, not embedded JSON. Next: help relationships or collection create --help."#,
        ["fields", "nullability"] => r#"A scalar without ! permits null; ! makes that scalar non-null. Choose requiredness from the data contract, including existing documents. ID! is unsupported; use String! for a required business identifier.
```graphql
type Note {
  title: String!
  summary: String
}
```
For arrays, [String!] forbids null elements; [String] permits them. The array representation does not encode outer-list requiredness, so do not rely on [String!]! to require the list itself. Adding a required field to existing data needs an explicit value strategy. Next: help fields arrays or help evolution additive."#,
        ["fields", "arrays"] => r#"Brackets declare an array. Scalar arrays support Boolean, Int, Float, Float32, Float64, String and DateTime; JSON, Blob and ID arrays are unsupported.
```graphql
type Reading {
  tags: [String!]
  samples: [Float32!]
  optionalCounts: [Int]
}
```
! inside brackets controls null elements. Arrays of collection types are relationships. A float array alone is not a vector index. Next: help relationships or help indexes vector."#,
        ["fields", "defaults"] => r#"@default supplies a value when a GraphQL create omits the field. Its value must match the field type; an explicit supplied value takes precedence.
```graphql
type Membership {
  role: String @default(value: "member")
  enabled: Boolean @default(value: true)
}
```
A schema default does not backfill historical documents and cannot invent a business identifier. Next: help evolution additive or collection create --help."#,
        ["fields", "crdt"] => r#"Ordinary fields use a last-write-wins register. Numeric counters merge accumulated changes: pcounter increases only; pncounter permits increases and decreases.
```graphql
type Inventory {
  quantity: Int @crdt(type: "pncounter")
}
```
Use counters for accumulated values, not identifiers or ordinary replaceable numbers. Counter types require numeric fields and cannot be indexed. Changing merge semantics affects existing data; inspect the deployed definition first. Next: collection get --help or help evolution migrations."#,
        ["fields", "immutable"] => r#"@immutable makes a field write-once at document creation. Later document updates must retain its value.
```graphql
type Submission {
  externalId: String! @immutable
  description: String
}
```
Immutability does not imply uniqueness or grant document access. A mistakenly assigned immutable value cannot be repaired by an ordinary update. Choose this only for values that must remain fixed. Next: help indexes unique or collection create --help."#,
        ["fields", "constraints"] => r#"@constraints(size: N) records array-size metadata; 0 means unspecified. It has no effect on scalar fields.
```graphql
type Article {
  tags: [String!] @constraints(size: 10)
}
```
The pinned runtime stores this metadata without enforcing array length on document writes. Do not use it as a business validation guarantee. Vector dimensions belong to the vector index definition. Next: help indexes vector or help fields nullability."#,
        ["relationships"] => "A collection-valued field links documents; an array gives the many side. @primary selects the side holding the foreign key, not a business primary key. Read help relationships TOPIC: one-to-many, one-to-one, named, self. Publish related new types together; inspect the resulting definition with collection get.",
        ["relationships", "one-to-many"] => r#"Use a scalar collection field on the owning document and an array on the reverse side.
```graphql
type Author { books: [Book] }
type Book { author: Author }
```
DefraDB makes Book.author primary and supplies _authorID for its document reference. A String containing an author name is not this relationship. One-to-many foreign keys are not automatically indexed; declare an index if needed. Next: help indexes ordered or collection create --help."#,
        ["relationships", "one-to-one"] => r#"For two scalar relationship fields, mark exactly one side @primary. That side holds the foreign key and receives a uniqueness index.
```graphql
type Person { profile: Profile @primary }
type Profile { person: Person }
```
Do not put @primary on both sides or on a business identifier to request uniqueness. A missing or conflicting primary side is a schema error; correct the relationship definition. Next: help relationships named or collection create --help."#,
        ["relationships", "named"] => r#"Use the same @relation name on both ends to identify a relationship, especially when the same collections have several links.
```graphql
type Author {
  books: [Book] @relation(name: "authorship")
}
type Book {
  author: Author @relation(name: "authorship")
}
```
Give distinct relationships distinct names. Names pair fields; they do not identify document instances. If pairing fails, inspect both endpoint types and their relation names. Next: help relationships one-to-one or collection create --help."#,
        ["relationships", "self"] => r#"A collection may reference itself, for example a hierarchy.
```graphql
type Category {
  name: String
  parent: Category @relation(name: "tree")
  children: [Category] @relation(name: "tree")
}
```
The scalar parent field holds the reference; children is the reverse relationship. This defines links, not a business rule forbidding cycles. Use named pairs when several self-relationships exist. Next: help relationships named or collection create --help."#,
        ["evolution"] => "Evolve a deployed collection with collection update; collection create installs new definitions or verifies matching ones. Read help evolution TOPIC: additive, versions, migrations. Inspect current fields and version IDs before changing them. Schema changes and config updates to writers are separate operations.",
        ["evolution", "additive"] => "Adding a nullable field usually needs no transformation: existing documents have no supplied value for it. Read collection get, then collection update --help for the JSON Patch shape. Defaults affect new creates; they do not fill historical rows. Update configured writers and readers separately. Replacing a collection through create is not an update. Required fields, renamed fields or changed value representations need an explicit data strategy; read help evolution migrations.",
        ["evolution", "versions"] => "A collection keeps its identity across versioned definitions. version list reveals exact VersionIDs and active state; version get inspects one. Activation selects a definition and may reindex through registered migrations; it does not roll documents back. For a transforming change, create the destination inactive, register its migration, then activate and materialize. Read version activate --help for that sequence. Not every property can be changed by JSON Patch: indexes use their own management operations, and this tool preserves collection names.",
        ["evolution", "migrations"] => "A lens transforms document values between collection versions. Nullable additions usually need none. For a value transformation, register compiled WASM with migration set using exact source and destination VersionIDs, then activate and materialize. The destination's previous version must match the source. This tool accepts inline module bytes, not source code or file paths. Materialization caches transformed documents without new document commits; it cannot invent missing business facts. Read migration set --help for inputs and version activate --help for ordering.",
        _ => return None,
    })
}

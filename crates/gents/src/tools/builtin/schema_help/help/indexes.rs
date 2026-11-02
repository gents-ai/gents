pub(super) fn page(path: &[&str]) -> Option<&'static str> {
    Some(match path {
        ["indexes"] => {
            r#"Indexes support ordered lookups, uniqueness, vector search and text search. Declare them in the SDL when creating a collection.
Discover: help indexes ordered | unique | composite | vector | fulltext | encrypted.
This tool does not expose index create/delete operations for existing collections. DefraDB rejects changing indexes through collection update; do not retry an index change as a schema patch."#
        }
        ["indexes", "ordered"] => {
            r#"An ordered index supports field-value lookups and ordering. ASC is the default direction; an ordinary index permits repeated values.
```graphql
type Event {
  occurred_at: DateTime @index(ordered: {direction: DESC})
}
```
Use @index without arguments for a default ordered index. name gives an index a stable name. Options belong inside ordered; vector options use a separate vector block.
Next: help indexes unique; help indexes composite."#
        }
        ["indexes", "unique"] => {
            r#"A unique index rejects duplicate indexed values. Use it for a key that identifies one record, not a group or correlation shared by many records.
```graphql
type Account {
  external_id: String @index(ordered: {unique: true})
}
```
For a compound key, uniqueness applies to the combination of indexed fields. Removing uniqueness from an existing index requires native index management, which this tool does not expose; collection update cannot change it.
Next: help indexes composite."#
        }
        ["indexes", "composite"] => {
            r#"A composite ordered index covers several fields in the declared order. Define it on the collection with ordered.includes. Each field may override the default direction.
```graphql
type Event @index(name: "by_project_time", ordered: {
  includes: [{field: "project"}, {field: "occurred_at", direction: DESC}]
}) {
  project: String
  occurred_at: DateTime
}
```
Add unique: true inside ordered only when the complete field combination must be unique. Vector indexes cover a single field and cannot use this collection-level form.
Next: help indexes unique; help indexes vector."#
        }
        ["indexes", "vector"] => {
            r#"A vector index finds similar numeric vectors. It does not generate embeddings. Declare one on a numeric array field with @index(vector: {...}); dimensions must be explicit and positive.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {dimensions: 768})
}
```
Defaults: HNSW algorithm, COSINE metric. Choose one algorithm block; it selects the algorithm and holds its settings. @vectorIndex is not supported.
Discover: help indexes vector dimensions | metrics | hnsw | flat | ivfpq | ivfflat | ssg.
For generated vectors: help embeddings. Similarity queries use the data/query interface, not schema commands."#
        }
        ["indexes", "vector", "dimensions"] => {
            r#"Dimensions is the number of numbers in each vector. Set it to the embedding model's output width, or the width of vectors you supply.
```graphql
type Point {
  coordinates: [Float32!] @index(vector: {dimensions: 3})
}
```
The value must be positive. DefraDB does not infer it from @embedding. Stored and query vectors must match the declared width; changing an embedding model to a different width also requires compatible storage and indexing.
Next: help embeddings; help indexes vector metrics."#
        }
        ["indexes", "vector", "metrics"] => {
            r#"COSINE compares direction; EUCLIDEAN compares straight-line distance; DOT uses dot product and is affected by magnitude. Match the metric to the embedding model and retrieval task. COSINE is the default.
```graphql
type Point {
  coordinates: [Float32!] @index(vector: {
    dimensions: 3, hnsw: {metric: EUCLIDEAN}
  })
}
```
Put metric inside the selected algorithm block. HNSW, FLAT and SSG support all three metrics. IVF_PQ and IVF_FLAT support COSINE only. Use the same metric when querying.
Next: help indexes vector hnsw; help indexes vector flat."#
        }
        ["indexes", "vector", "hnsw"] => {
            r#"HNSW searches an approximate neighbor graph. It is the default vector algorithm.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {
    dimensions: 768, hnsw: {metric: COSINE}
  })
}
```
Defaults: M: 16 connections above layer zero; efConstruction: 128 build candidates; efSearch: 64 search candidates. More candidates generally trade work for recall. Start with defaults before tuning. Supports COSINE, EUCLIDEAN and DOT.
Next: help indexes vector metrics; help indexes vector flat."#
        }
        ["indexes", "vector", "flat"] => {
            r#"FLAT compares every indexed vector. It gives exact results with linear search cost and needs no graph or clustering parameters. Useful for small collections or checking approximate-search recall.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {
    dimensions: 768, flat: {metric: COSINE}
  })
}
```
Supports COSINE, EUCLIDEAN and DOT.
Next: help indexes vector hnsw; help indexes vector metrics."#
        }
        ["indexes", "vector", "ivfpq"] => {
            r#"IVF_PQ searches coarse clusters using compressed vector codes. It trades precision for smaller stored representations. COSINE is the only supported metric.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {dimensions: 768, ivfpq: {}})
}
```
Defaults derive nlist from corpus size and m from vector width. nprobe: 8 controls clusters searched; sampleBytes: 134217728 caps the training sample. Explicit m selects subquantizers and must be a positive divisor of dimensions. These are training/search settings, not embedding-provider options.
Next: help indexes vector ivfflat; help indexes vector dimensions."#
        }
        ["indexes", "vector", "ivfflat"] => {
            r#"IVF_FLAT searches coarse clusters while retaining full-precision vectors. Results can miss neighbors outside the searched clusters. COSINE is the only supported metric.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {dimensions: 768, ivfflat: {}})
}
```
Defaults derive nlist from corpus size; nprobe: 8 controls clusters searched; sampleBytes: 134217728 caps the training sample. It has no m option because vectors are not product-quantized.
Next: help indexes vector ivfpq; help indexes vector flat."#
        }
        ["indexes", "vector", "ssg"] => {
            r#"SSG uses a single-layer neighbor graph with edges pruned by angle. Searches are approximate.
```graphql
type Note {
  embedding: [Float32!] @index(vector: {dimensions: 768, ssg: {}})
}
```
Defaults: R: 50 maximum retained edges per node; angle: 60 degrees between retained edges; pool: 100 search candidates. Supports COSINE, EUCLIDEAN and DOT.
Next: help indexes vector hnsw; help indexes vector metrics."#
        }
        ["indexes", "fulltext"] => {
            r#"@fulltext declares a BM25 text-search index. It ranks matching words rather than embedding similarity.
```graphql
type Article {
  body: String @fulltext(language: "english")
}
```
Defaults: language: "english", k1: 1.2 for term-frequency saturation, b: 0.75 for document-length normalization. Leave scoring parameters at defaults unless you have measured retrieval needs. Run text queries through the data/query interface.
Next: help indexes vector; help embeddings."#
        }
        ["indexes", "encrypted"] => {
            r#"@encryptedIndex declares an equality index for searchable encryption.
```graphql
type Contact {
  email: String @encryptedIndex
}
```
The declaration alone does not configure encryption keys or grant permission to read protected documents. Those require the runtime's encryption and authorization facilities. Do not assume ordered, range or vector search from an encrypted equality index.
Next: help indexes ordered."#
        }
        ["embeddings"] => {
            r#"@embedding generates vectors from named source fields when documents are written. Vector indexing is a separate declaration.
```graphql
type Note {
  text: String
  embedding: [Float32!] @embedding(
    provider: "openai", model: "text-embedding-3-small", fields: ["text"]
  )
}
```
The output field must be [Float32!]; providers are openai and ollama. Model/provider access must work on the runtime node. fields must be nonempty and cannot reference this or another embedding field. Supply vectors directly when generation happens elsewhere.
Discover: help embeddings generation; help embeddings indexing."#
        }
        ["embeddings", "generation"] => {
            r#"Embedding generation may call a provider during a document write. provider, model and optional url select that service; fields identifies the source content. Credentials come from node configuration, not the SDL.
Source fields must be scalar String, Int, Float/Float64, Float32, Boolean, DateTime or Blob. JSON, arrays and relationships are unsupported. On create, explicitly supplying the vector skips generation. On update, changing a source field regenerates it unless the vector is explicitly supplied. Verify provider credentials, reachability and generated values before relying on retrieval.
Next: help embeddings indexing. Inspect documents through the data/query interface when testing generation."#
        }
        ["embeddings", "indexing"] => {
            r#"Combine generation and indexing only when the declared dimensions match the chosen model's output width.
```graphql
type Note {
  text: String
  embedding: [Float32!]
    @embedding(provider: "openai", model: "your-model", fields: ["text"])
    @index(vector: {dimensions: 768})
}
```
Replace your-model and 768 with the configured model and its output width. DefraDB requires dimensions explicitly; @embedding does not fill them in. Keep provider configuration, supplied vectors and query vectors consistent.
Next: help indexes vector dimensions; help indexes vector metrics."#
        }
        _ => return None,
    })
}

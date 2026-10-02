pub(super) fn page(path: &[&str]) -> Option<&'static str> {
    Some(match path {
        ["collections"] => "An SDL type defines a document collection. Fields describe values; collection directives affect authorization, identity and history. Read help collections TOPIC: policy, branchable, governed, downsample. Query-backed collections: help views. Ordinary collections must be materialized. Publish with collection create; use view create when a source query is required.",
        ["collections", "policy"] => r#"Collection-level @policy attaches an existing DefraDB ACP policy and its named resource.
```graphql
type Report @policy(id: "POLICY_ID", resource: "report") {
  title: String
}
```
Replace POLICY_ID with a registered policy ID and report with a resource in that policy. This declares the authorization contract; it does not create the policy or grant access. Existing bindings cannot be changed through schema patching. Field-level @policy is not implemented; materialized views cannot carry an ACP policy. Next: collection create --help or help views virtual."#,
        ["collections", "branchable"] => r#"@branchable tracks collection history as one verifiable entity, adding collection-level commits for document changes.
```graphql
type Event @branchable {
  recordedAt: DateTime
  message: String
}
```
Use it when collection history is part of the application contract. With ACP, authorization registration uses the stable collection ID. This flag does not create a working copy or expose a branch-management command. DefraDB refuses changing IsBranchable after creation. Next: help collections policy or collection create --help."#,
        ["collections", "governed"] => r#"@governed binds a collection's identity to a governance root.
```graphql
type Record @governed(root: "GOVERNANCE_ROOT") {
  key: String! @immutable
  value: String
}
```
Replace GOVERNANCE_ROOT with the application's existing self-addressing root identifier, not a public key or an invented label. It participates in collection identity and survives signing-key rotation. The directive does not install a merge validator or grant permissions; governance requires the application's matching runtime support. This tool cannot provision that support. Next: help fields immutable or collection create --help."#,
        ["collections", "downsample"] => "@downsample derives time-window aggregates from source history. Create it with view create and a source query; the SDL directive is @downsample(interval: \"1h\", timeField: \"recordedAt\", retention: \"168h\"). For raw source data, select the DateTime time field and one numeric measure field. The target needs source_doc_id: String, source_height: Int, window_start: DateTime, window_end: DateTime, and at least one aggregate: count: Int; avg: Float; or numeric sum, min, max. Other target fields must be selected from the source with matching types. Do not copy the time or measure field as an extra target field. Intervals and optional retention must be positive. Native refresh/history-GC operations are not exposed here. Next: view create --help or help views refresh.",
        ["views"] => "A view names the result of a source query using a target SDL definition. Read help views TOPIC: virtual, materialized, refresh. The target fields must match the source selection. Publish both query and SDL through view create, not collection create. A view is not a grant to read its source documents. Time-window aggregation: help collections downsample.",
        ["views", "virtual"] => "A virtual view computes its result from the source query when read. In the target SDL, use @materialized(if: false), for example type ArticleTitles @materialized(if: false) { title: String }, with source query Article { title }. Publish them together with view create. A non-materialized type without a source query is invalid. This is useful when results should reflect current source data without a stored view cache. A virtual view may carry an existing ACP policy; source access still matters. Next: view create --help or help collections policy.",
        ["views", "materialized"] => "A materialized view stores a cache of the source query result. Materialization defaults to true; @materialized makes it explicit. Example target SDL: type ArticleTitles @materialized { title: String }; source query: Article { title }. view create builds the initial cache. Do not assume it stays current after source changes: refreshing the cache is a separate native operation. Materialized views cannot carry an ACP policy. Next: view create --help or help views refresh.",
        ["views", "refresh"] => "View refresh rebuilds a materialized view's cache from its source query. DefraDB supports it, but this schema tool does not expose a refresh command. Native downsample history GC is also unavailable here. collection materialize is different: it advances cached documents through schema-version migrations; it does not refresh a view or recompute its source query. Do not substitute it for view refresh. Use a virtual view when query-time computation fits the task and no refresh path is available. Next: help views virtual or help evolution migrations.",
        _ => return None,
    })
}

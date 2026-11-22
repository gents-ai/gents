/-!
# Which collections `schema collection list` names

Decision (maintainer, 2026-11-22): a model reviewing its own tools found the
listing's application collections buried among about ninety the runtime
manages, which only a Gents release changes. The listing names the
application collections; the runtime's are listed only on request
(`options.scope: "all"`).

Rust owner: `crates/gents-tools/src/builtin/schema_help/mod.rs`
(`lists_collection`).
-/

namespace SchemaCollections

def listed (includeRuntime runtimeManaged : Bool) : Bool :=
  includeRuntime || !runtimeManaged

theorem application_collections_are_always_listed (includeRuntime : Bool) :
    listed includeRuntime false = true := by
  cases includeRuntime <;> rfl

theorem runtime_collections_are_listed_only_on_request (includeRuntime : Bool) :
    listed includeRuntime true = includeRuntime := by
  cases includeRuntime <;> rfl

end SchemaCollections

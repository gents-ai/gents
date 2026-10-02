/// SDK build guidance uses debug WASM because optimized lens_sdk 0.8 transport
/// buffers can be corrupted (https://github.com/sourcenetwork/lens/issues/166).
pub(super) fn page(path: &[&str]) -> Option<&'static str> {
    Some(match path {
        ["migration", "workflow"] => {
            r#"A lens transforms existing document values between collection versions. Use one for a derived field or representation change; a nullable field addition alone usually needs none.
1. Inspect collection get NAME and version list NAME. Record the source VersionID.
2. Compile and test the lens outside this tool. Read help migration authoring.
3. Preview collection update with the destination fields and /NAME/IsActive=false in the same patch. Apply next_call and record the new VersionID.
4. Preview migration set with source, destination and compiled module; apply next_call.
5. Inspect version get DESTINATION, then preview/apply version activate DESTINATION and collection materialize NAME.
6. Verify representative documents through an authorized data/query tool.
Schema preview inspects intent; it does not execute the transform on sample documents. This tool can register modules, but cannot write source files, compile code or query document values. Those steps require separately available tools.
Next: help migration build; help migration verify."#
        }
        ["migration", "authoring"] => {
            r#"Write a deterministic transformation from one document object to one document object. Preserve _docID, _deleted and unrelated fields. Define how missing, null and unexpected input values are handled; do not invent historical business values.
The Rust lens_sdk supplies allocation, stream handling and exported entry points. Keep transformation logic separate enough to test with ordinary JSON before compiling WASM. A reverse transformation is optional, but required if documents must be read through the transition in reverse.
Discover: help migration authoring rust for complete source; help migration build for its Cargo manifest and build commands; help migration arguments for configurable modules; help migration inverse for reverse semantics.
The schema tool accepts compiled bytes only. If file editing and compilation tools are unavailable, obtain a tested module from the user or an authorized build agent; do not fabricate base64 or substitute source code for WASM."#
        }
        ["migration", "authoring", "rust"] => {
            r#"Complete src/lib.rs example: the source has name: String; the destination adds label: String. Forward copies name into label; inverse drops label. Both preserve other keys, including document identity. Supply actual source/destination VersionIDs when registering.
```rust
use std::{collections::HashMap, error::Error};
use lens_sdk::StreamOption;
use serde_json::Value;

type Doc = HashMap<String, Value>;
type Input<'a> = &'a mut dyn Iterator<Item = lens_sdk::Result<Option<Doc>>>;
type Output = Result<StreamOption<Doc>, Box<dyn Error>>;

#[cfg(target_arch = "wasm32")]
lens_sdk::define!(forward, backward);

pub fn add_label(mut doc: Doc) -> Result<Doc, Box<dyn Error>> {
    let name = doc.get("name").and_then(Value::as_str)
        .ok_or("name must be a string")?.to_owned();
    doc.insert("label".into(), Value::String(name));
    Ok(doc)
}

pub fn remove_label(mut doc: Doc) -> Doc {
    doc.remove("label");
    doc
}

fn forward(iter: Input<'_>) -> Output {
    match iter.next() {
        Some(item) => match item? {
            Some(doc) => Ok(StreamOption::Some(add_label(doc)?)),
            None => Err("expected a document object".into()),
        },
        None => Ok(StreamOption::EndOfStream),
    }
}

fn backward(iter: Input<'_>) -> Output {
    match iter.next() {
        Some(item) => match item? {
            Some(doc) => Ok(StreamOption::Some(remove_label(doc))),
            None => Err("expected a document object".into()),
        },
        None => Ok(StreamOption::EndOfStream),
    }
}
```
Adapt the business transformation and test its accepted input domain. This example overwrites label in the forward direction and discards it in reverse; it is not a general data rollback.
Next: help migration build; help migration verify."#
        }
        ["migration", "build"] => {
            r#"For the source in help migration authoring rust, create a standalone Cargo project with this Cargo.toml:
```toml
[package]
name = "schema_lens"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
lens_sdk = "=0.8.1"
serde_json = "1"
```
Using separately granted file/bash tools, build from that project's directory:
```sh
rustup target add wasm32-unknown-unknown
cargo test
cargo build --target wasm32-unknown-unknown
python3 -c 'import base64,pathlib; print(base64.b64encode(pathlib.Path("target/wasm32-unknown-unknown/debug/schema_lens.wasm").read_bytes()).decode())' > lens.b64
```
Use the actual base64 file contents as Module. Keep the source and Cargo.lock with the artifact. Add tests before relying on cargo test: the template contains no tests by itself. Use debug WASM with lens_sdk 0.8; optimized builds have a known transport-buffer defect. No WASI filesystem, network or console imports are supplied by this lens host.
Next: migration set --help; help migration verify."#
        }
        ["migration", "contract"] => {
            r#"A lens module consumes a stream of JSON values and returns one framed result per call. DefraDB schema migration uses document objects; design one output object for each input document.
The wasm32 host provides the import lens.next: () -> i32. Export memory and alloc: (i32) -> i32; export transform: () -> i32 for forward execution. Export inverse: () -> i32 when reverse execution is required. Nonempty Arguments additionally require set_param: (i32) -> i32. Integers here carry byte lengths or offsets into module memory, not JSON values.
The SDK in help migration authoring rust generates these entry points. The host repeatedly calls transform or inverse until end-of-stream; returning the same document forever causes a loop. Registration compiles WASM but does not prove the exports work on real inputs.
Discover: help migration contract memory for framing; help migration contract documents for document rules. Next: help migration arguments; help migration verify."#
        }
        ["migration", "contract", "memory"] => {
            r#"The native WASM transport uses byte offsets into exported memory. A framed JSON result is: one type byte 1, four bytes containing payload length as unsigned little-endian u32, then that many UTF-8 JSON bytes. The total allocation is 5 + payload length.
Type byte 127 alone marks end-of-stream. Type byte 255 (signed -1) followed by length and UTF-8 text reports an error. Type byte 0 represents a null item; it is not an empty document. A returned pointer of zero ends output, so it cannot identify a result buffer.
lens.next() obtains an input frame allocated through the module's alloc. set_param receives the same JSON framing and returns an ok/error frame. Buffers must remain valid while the host reads them; returning a pointer into freed memory is invalid. Use the SDK instead of hand-writing memory management unless implementing another language binding.
Next: help migration authoring rust; help migration contract documents."#
        }
        ["migration", "contract", "documents"] => {
            r#"A migration input is a JSON object containing document fields and reserved metadata such as _docID and _deleted. Preserve those keys and unrelated values while changing the intended fields. Produce field names and value types compatible with the destination definition.
For a rename, move the existing value deliberately; decide what happens if the old field is missing or the destination field already exists. For a derived field, use existing facts and a specified rule. Missing historical information requires an explicit application decision.
Return one object per input document. The general Lens engine supports stream transforms, but DefraDB's per-document schema migration is not a pipeline for filtering, fan-out or aggregating records. No output fails migration; a null, array or scalar output fails the document-object contract.
An inverse cannot reconstruct information the forward transform discarded unless that information is retained elsewhere in the document contract.
Next: help migration inverse; help migration verify."#
        }
        ["migration", "arguments"] => {
            r#"Arguments is static JSON passed to each module instance before its documents. Its keys are defined by that module, not by DefraDB. Modules in Lenses have separate arguments and execute in listed order.
For a Rust module parameterized by source and destination field names, replace its define! call and add:
```rust
use std::sync::RwLock;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Parameters { pub src: String, pub dst: String }
static PARAMETERS: RwLock<Option<Parameters>> = RwLock::new(None);

#[cfg(target_arch = "wasm32")]
lens_sdk::define!(PARAMETERS: Parameters, forward, backward);
```
Add serde = { version = "1", features = ["derive"] } to Cargo.toml. In each callback read PARAMETERS, require it to be set, and apply the requested mapping. The macro exports set_param; it does not implement the mapping for you. Register Arguments: {"src":"name","dst":"label"} only for a module implementing that contract.
Omitted/null Arguments or an empty object skips set_param. Other JSON values invoke it; missing exports or parameter decoding errors fail execution.
Next: help migration contract; migration set --help."#
        }
        ["migration", "inverse"] => {
            r#"inverse transforms a destination-version document back into the source representation. Implement it only when its meaning is defined. A lossy transform is not made reversible merely by exporting a function.
For the example in help migration authoring rust, forward adds label from name and backward removes label while retaining name. That round-trip preserves source documents whose original contract had no label; it does not preserve independently edited destination labels.
Lenses entries normally use Inverse:false, selecting transform for forward traversal. Inverse:true selects that module's inverse instead; it does not generate an inverse. When traversing the version history backward, DefraDB reverses module order and flips each entry's direction.
A module without inverse may register successfully and later fail when reverse traversal is attempted. Test the reverse direction before activating an older version. Activation selects a representation, not an earlier snapshot of document history.
Next: help migration versions; help migration verify."#
        }
        ["migration", "versions"] => {
            r#"Use exact VersionIDs from version list NAME, not collection names or invented IDs. Source and destination must already exist for this tool. Read version get for both and inspect the destination's PreviousVersion.SourceCollectionID: when present, it must match the migration source VersionID.
For several transitions, register the transformation for each adjacent edge. DefraDB composes the path when reading older or newer document representations; do not attach one transform to unrelated versions to bypass an adjacency error.
Keep the destination inactive while preparing a transformation. Include /NAME/IsActive=false in the same collection update patch that creates its new fields. Register migration, inspect the saved transform metadata, then activate. Use collection materialize when you need to advance cached documents proactively.
Changing the active version does not undo document edits. Reverse traversal requires the corresponding inverse functions when transformations exist.
Next: help migration workflow; help migration inverse."#
        }
        ["migration", "verify"] => {
            r#"Verify the module before publication with ordinary unit tests over JSON objects. Cover missing/null/wrong-type inputs, already-populated destination fields, unrelated fields and _docID preservation. If reverse traversal is required, test inverse(forward(source)) over the intended source domain.
Also exercise the compiled WASM through a native lens host against representative documents; read help migration verify host for commands. Pure Rust tests cannot detect missing WASM exports, invalid memory framing or SDK build-mode defects. Use an isolated collection/version pair for integration checks before changing production data.
migration set success proves registration, not successful transformation of every row. version get confirms definitions and transform links; it does not inspect document values. Activation, materialization and data queries may execute transforms and expose data-dependent errors. Read outputs through an authorized data/query tool when execution is in scope.
If only configuration is requested, inspect the saved definition and state which runtime verification remains undone.
Next: help migration recovery; help migration contract."#
        }
        ["migration", "verify", "host"] => {
            r#"If a compatible DefraDB Rust source checkout and build tools are available, its lens-host utility executes compiled modules without a database. Create lens-check.json with an absolute artifact path:
```json
{"Lenses":[{"Path":"/absolute/path/schema_lens.wasm","Arguments":{},"Inverse":false}]}
```
Create input.json:
```json
[{"_docID":"example-id","name":"Ada"}]
```
From that checkout, run:
```sh
cargo run -p lens-host -- /absolute/path/lens-check.json < /absolute/path/input.json
```
For the example lens, expect one object preserving _docID and name and adding label:"Ada". To test backward, set Inverse:true and supply the forward output; label should be removed. Include malformed and boundary inputs in separate tests.
Path is allowed by this standalone test utility; Gents migration set still requires inline base64 Module. The utility has no schema validation or database sandbox limits. Database execution limits each WASM batch to 64 MiB memory and 1,000,000 fuel, so a successful host test does not replace an isolated database check.
Next: help migration workflow; help migration recovery."#
        }
        ["migration", "recovery"] => {
            r#"Start by locating the failure: preview, registration, activation, materialization or document read. Inspect version list and version get before changing anything; an execution error does not mean the destination definition was never created.
Invalid base64 or WASM: rebuild and encode the actual artifact. Missing memory/alloc/transform/inverse/set_param or invalid type IDs: check the module ABI and use the supported debug build. A module-reported error: reproduce with the failing document and arguments, then fix the transformation. Fuel/memory exhaustion: bound the computation and allocations; use help migration verify host to isolate the module. An adjacency error: use the actual source/destination edge. A stale digest: preview the intended call again.
Register corrected module bytes for the intended edge through a new migration preview, then apply its next_call. Inspect the saved transform link before retrying authorized execution. Already materialized documents may be at the destination version; replacing a lens does not automatically reapply it to them. Do not assume switching versions is a safe rollback without a valid inverse.
Next: help migration verify; help migration versions."#
        }
        _ => return None,
    })
}

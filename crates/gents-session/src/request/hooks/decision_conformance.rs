use super::effect::{DocRef, Manifest, OwnedCollection};
use super::hook::{history_permitted, Query};

#[test]
fn history_reads_follow_the_lean_history_bound() {
    gents_lean_vocab::families::hook_decisions_history::replay(|case| {
        let leak = |text: &str| -> &'static str { Box::leak(text.to_owned().into_boxed_str()) };
        let manifest = Manifest {
            name: "plugin",
            owns: case
                .owns
                .iter()
                .map(|collection| OwnedCollection {
                    name: leak(collection),
                    schema: None,
                    grants: &[],
                })
                .collect(),
            bound: 0,
            history: case
                .history
                .iter()
                .map(|collection| leak(collection))
                .collect(),
        };
        let named = case
            .named
            .iter()
            .map(|named| DocRef::new(named.collection.clone(), named.doc.clone()))
            .collect::<Vec<_>>();
        let mut query = Query::select(case.read.collection.clone(), "status").revisions("");
        if let Some(doc) = &case.read.doc {
            query = query.eq("_docID", doc.clone());
        }
        gents_lean_vocab::families::hook_decisions_history::HistoryOutput {
            permitted: history_permitted(&manifest, &named, &query),
        }
    });
}

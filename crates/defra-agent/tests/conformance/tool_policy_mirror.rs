//! Pure Rust mirror of the Lean ToolPolicy meet on the reduced SurfaceView.
//! SP1-Rust replaces this with the production resolver pointed at the same
//! generated cases.

use crate::lean_vocab_test::LeanToolPolicySurfaceView as View;

fn meet_allowed_kind(a: &str, b: &str) -> String {
    match (a, b) {
        ("none", _) | (_, "none") => "none",
        ("all", x) => x,
        (x, "all") => x,
        _ => "only",
    }
    .to_string()
}

fn meet2(a: &View, b: &View) -> View {
    View {
        file_rank: a.file_rank.min(b.file_rank),
        meta: a.meta && b.meta,
        defra_query: a.defra_query && b.defra_query,
        spawn: a.spawn && b.spawn,
        bash_mode: a.bash_mode.min(b.bash_mode),
        bash_net: a.bash_net.min(b.bash_net),
        bash_sandbox: a.bash_sandbox && b.bash_sandbox,
        bash_allowed_kind: meet_allowed_kind(&a.bash_allowed_kind, &b.bash_allowed_kind),
        mcp_probe: a.mcp_probe.clone(),
        mcp_permits: a.mcp_permits && b.mcp_permits,
        write_fields: {
            let b_fields: std::collections::BTreeSet<_> = b.write_fields.iter().cloned().collect();
            let mut fields = a
                .write_fields
                .iter()
                .filter(|field| b_fields.contains(*field))
                .cloned()
                .collect::<Vec<_>>();
            fields.sort();
            fields
        },
    }
}

pub(super) fn rederive(behavior: &View, ceiling: &View, runtime: &View) -> View {
    meet2(&meet2(behavior, ceiling), runtime)
}

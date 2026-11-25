use super::support::{
    cfg_test, item_cfg_test, named_exceptions, path_name, production_site_counts,
};
use syn::visit::{self, Visit};
use syn::ExprCall;

/// A request a client endpoint authorizes is sent through
/// `GraphqlEndpoint::send_resolving`, so a refusal of its credential reads the
/// home's status again and is sent once more (`Identity.HeldAudience.resend`),
/// or inside a transaction through `authorize_in_transaction`, whose
/// transaction recovers as a unit (`Identity.HeldAudience.resendUnit`).
/// Each exception names why its direct `authorize` is correct.
#[rustfmt::skip]
const DIRECT_AUTHORIZE_EXCEPTIONS: &[(&str, usize, &str)] = &[
    ("crates/gents-store/src/config_client/client_authentication.rs", 1, "correct by design: the body of the authorizer authorize_runtime, whose callers are counted"),
    ("crates/gents-store/src/config_client/graphql.rs", 2, "correct by design: a transaction's commit and discard are never sent alone again; the transaction reruns as a unit and a refused commit is told (Identity.HeldAudience.resendUnit, a_refused_commit_is_never_run_again)"),
    ("crates/gents-store/src/config_client/mod.rs", 1, "correct by design: the body of the authorizer authorize_in_transaction, whose callers recover as a unit (resendUnit)"),
];

/// What authorizes a request as the endpoint's principal.
const AUTHORIZERS: &[&str] = &["authorize", "authorize_runtime"];

/// What sends the request a closure builds through `send_resolving`.
const RESOLVING_SENDERS: &[&str] = &[
    "send_resolving",
    "send_counting_resends",
    "endpoint_get_json",
    "endpoint_send",
];

#[derive(Default)]
struct UnrecoveredAuthorizeVisitor {
    resolving: usize,
    sites: usize,
}

impl<'ast> Visit<'ast> for UnrecoveredAuthorizeVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if !self.visit_resolving_call(call) {
            visit::visit_expr_call(self, call);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if AUTHORIZERS.contains(&call.method.to_string().as_str())
            && call.args.len() == 1
            && self.resolving == 0
        {
            self.sites += 1;
        }
        if RESOLVING_SENDERS.contains(&call.method.to_string().as_str()) {
            self.visit_expr(&call.receiver);
            self.resolving += 1;
            for argument in &call.args {
                self.visit_expr(argument);
            }
            self.resolving -= 1;
        } else {
            visit::visit_expr_method_call(self, call);
        }
    }
}

impl UnrecoveredAuthorizeVisitor {
    fn visit_resolving_call(&mut self, call: &ExprCall) -> bool {
        if !path_name(call).is_some_and(|name| {
            RESOLVING_SENDERS
                .iter()
                .any(|sender| name.ends_with(sender))
        }) {
            return false;
        }
        self.resolving += 1;
        for argument in &call.args {
            self.visit_expr(argument);
        }
        self.resolving -= 1;
        true
    }
}

fn unrecovered_authorize_sites(syntax: &syn::File) -> usize {
    let mut visitor = UnrecoveredAuthorizeVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn client_requests_recover_a_refused_credential() {
    named_exceptions(
        "refused credential recovery",
        DIRECT_AUTHORIZE_EXCEPTIONS,
        &production_site_counts(|_, syntax| unrecovered_authorize_sites(syntax)),
        "request(s) authorized outside `send_resolving`",
        "send through it so a refused credential recovers",
    );
}

#[test]
fn credential_recovery_fence_counts_authorize_outside_send_resolving() {
    for (source, sites) in [
        ("fn f() { e.authorize(c.get(u))?.send(); }", 1),
        ("fn f() { e.send_resolving(|e| e.authorize(c.get(u))); }", 0),
        (
            "fn f() { e.send_resolving(|e| Ok(e.authorize(c.get(u))?.json(&b))); }",
            0,
        ),
        ("fn f() { e.authorize_in_transaction(c.get(u), id); }", 0),
        ("fn f() { e.authorize_runtime(c.get(u)).await?.send(); }", 1),
        (
            "fn f() { e.send_resolving(|e| e.authorize_runtime(c.get(u))); }",
            0,
        ),
        (
            "fn f() { http_client::endpoint_get_json(&e, |e| e.authorize(c.get(u))); }",
            0,
        ),
        (
            "fn f() { endpoint_send(&e, |e| e.authorize(c.post(u))); }",
            0,
        ),
        ("fn f() { other(&e, |e| e.authorize(c.post(u))); }", 1),
        (
            "fn f() { e.authorize(c.get(u)).and(e.send_resolving(|e| e.authorize(r))); }",
            1,
        ),
        ("#[cfg(test)] mod tests { fn f() { e.authorize(r); } }", 0),
        (
            "impl E { #[cfg(test)] fn f(&self) { self.authorize(r); } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(unrecovered_authorize_sites(&syntax), sites, "{source}");
    }
}

/// A client keeps a delegation audience or clock only from a status its
/// home's node key signed for the read (`Identity.HeldAudience.reportedOfHome`):
/// what a status's unsigned fields report is read into a report only where
/// the home signs it, and a client takes a report only from the signed
/// statement (`home_status::authenticate`). Each exception names its reader
/// and why it is correct.
#[rustfmt::skip]
const STATUS_REPORT_READERS: &[(&str, usize, &str)] = &[
    ("crates/gents-protocol/src/home_attestation.rs", 2, "correct by design: the home reading its own status into the statement it signs (HomeStatement::of_status), and the report of a statement whose signature was checked (HomeStatement::reported)"),
];

/// What reads a report from a status, and the report itself.
const STATUS_REPORT_CALLS: &[&str] = &[
    "ReportedHome::observe",
    "ReportedDelegationAudience::observe",
];

#[derive(Default)]
struct StatusReportVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for StatusReportVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if path_name(call).is_some_and(|name| {
            STATUS_REPORT_CALLS
                .iter()
                .any(|reader| name.ends_with(reader))
        }) {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_struct(&mut self, report: &'ast syn::ExprStruct) {
        if report
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "ReportedHome")
        {
            self.sites += 1;
        }
        visit::visit_expr_struct(self, report);
    }
}

fn status_report_sites(syntax: &syn::File) -> usize {
    let mut visitor = StatusReportVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn a_client_keeps_only_what_its_home_signed() {
    named_exceptions(
        "signed status",
        STATUS_REPORT_READERS,
        &production_site_counts(|_, syntax| status_report_sites(syntax)),
        "read(s) of a status's unsigned report",
        "keep only what `home_status::authenticate` returns",
    );
}

#[test]
fn signed_status_fence_counts_unsigned_report_readers() {
    for (source, sites) in [
        ("fn f() { ReportedHome::observe(&status); }", 1),
        (
            "fn f() { gents_protocol::delegation_audience::ReportedDelegationAudience::observe(&s); }",
            1,
        ),
        ("fn f() { ReportedHome { audience, provider_time }; }", 1),
        (
            "fn f() { home_status::authenticate(&status, node, &nonce); }",
            0,
        ),
        (
            "#[cfg(test)] mod tests { fn f() { ReportedHome::observe(&s); } }",
            0,
        ),
        (
            "impl E { #[cfg(test)] fn f(&self) { ReportedHome { audience, provider_time }; } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(status_report_sites(&syntax), sites, "{source}");
    }
}

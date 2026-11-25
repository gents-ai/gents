use super::TtlOutcome;

/// Lean `RequestContext.PendingDisposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingDisposition {
    Expired,
    Open,
}

/// Lean `RequestContext.pendingDisposition`.
pub(crate) fn pending_disposition(ttl: &TtlOutcome) -> PendingDisposition {
    match ttl {
        TtlOutcome::Expired(_) => PendingDisposition::Expired,
        TtlOutcome::NotSet | TtlOutcome::Live(_) | TtlOutcome::Malformed(_) => {
            PendingDisposition::Open
        }
    }
}

#[cfg(test)]
mod conformance {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum TtlObservation {
        NotSet,
        Live,
        Expired,
        Malformed,
    }

    fn valid_until(ttl: &TtlObservation) -> Option<&'static str> {
        match ttl {
            TtlObservation::NotSet => None,
            TtlObservation::Live => Some("2026-09-01T00:00:01Z"),
            TtlObservation::Expired => Some("2026-08-31T23:59:59Z"),
            TtlObservation::Malformed => Some("not-a-time"),
        }
    }

    #[test]
    fn pending_disposition_matches_lean() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        gents_lean_vocab::families::request_context_pending_disposition::replay(
            |i: &gents_lean_vocab::families::request_context_pending_disposition::Input<
                TtlObservation,
            >| {
                super::pending_disposition(&super::super::parse_valid_until(
                    valid_until(&i.ttl),
                    now,
                ))
            },
        );
    }
}

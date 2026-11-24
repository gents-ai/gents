import Proofs.Store.TransactionFence

/-! The enrollment route write against a concurrent authorization revision.

The enrollment reconcile writes a member's route (`PeerPairingDesired`, source
`enrollment`) in a transaction that first checks, on the enrollment
documents, that the member's authorization generation is still current. An
approval, a renewal or a revocation commits a new
`NetworkAuthorizationRevision`: an arrival, which the reconcile's filtered
read of the enrollment documents does not fence (`TransactionFence`).

The fence is `SingleWriter`: every revision this node writes is decided and
committed under the enrollment decision lock, and the route write holds the
same lock from its authorization read through its commit. External premise:
a home admits one runtime process at a time (the store's advisory lock,
`boundary.plugin-job.single-step-driver`), so that lock orders the route
write against every revision written on this node.

A revision that arrives by merge (an admin-signed revision replicated back
to the node that signed it) takes no lock, so neither the lock nor any read
of the route write fences it. What covers it: the reconcile subscribes to the
revision collection, so the merge wakes its next pass, which deletes a route
the authority no longer names; and the route is a witness, never authority:
the pairing reconciler derives every install from its own fresh read of the
authority (`PairingReconcile.DataPlane`), so nothing is installed toward the
member meanwhile.

A route receipt (`record_applied_route`) is recorded on a projection read
outside any lock, so a receipt for a generation revoked meanwhile can commit.
It grants nothing: a receipt is evidence of an applied route, and terminal
delivery re-checks that the receipt's generation is current before it acts
(`Idempotent`).

Enrollment request admission reads the member's authorization outside the
claim transaction, but a member's request names another actor's Agent, and
cross-actor enrollment claims are unavailable: such a request is neither
claimed nor denied, so no admission decision rests on that read. -/

namespace Enrollment.RouteFence

open TransactionFence

abbrev routeDoc : Doc := 0

/-- The route write: a filtered read of the enrollment documents, then the
route. -/
def routeWrite (returned : List Doc) : Footprint :=
  { (Footprint.empty.read (.filtered returned)) with writes := [routeDoc] }

/-- A revision creates one fresh document. -/
def revisionWrite (revision : Doc) : Footprint :=
  { Footprint.empty with writes := [revision] }

/-- The store does not order them: a revision committed after the route write
began, a fresh document the route write neither read by id nor wrote, does
not abort it. So the route write and the revision must be ordered by their
owners, which the decision lock does for every revision this node writes. -/
theorem a_filtered_read_does_not_fence_a_revision (returned : List Doc) (revision : Doc)
    (h_fresh : revision ≠ routeDoc) :
    fencedAgainst (routeWrite returned) (revisionWrite revision) = false := by
  simp [fencedAgainst, conflicts, routeWrite, revisionWrite, Footprint.read, Footprint.empty,
    h_fresh, Ne.symm h_fresh]

/-- A merged revision takes no lock: the route write's read can miss it, and
the route stands until the pass the merge wakes. -/
theorem a_merged_revision_is_fenced_by_neither (returned : List Doc) (revision : Doc)
    (h_fresh : revision ≠ routeDoc) :
    fencedAgainst (routeWrite returned) (revisionWrite revision) = false :=
  a_filtered_read_does_not_fence_a_revision returned revision h_fresh

end Enrollment.RouteFence

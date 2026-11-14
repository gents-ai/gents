/-! # Network grants a pack may declare

A pack declares its plugin's network grant and HTTP timeout, and the host
checks the declaration when the pack is validated. The host serves only
allow-list entries for hostnames and IP literals, never raw TCP. A wildcard
names a domain, so it cannot name an IP literal. A timeout of zero is refused.

Entry text is abstracted to what decides servability: whether the entry is a
wildcard and whether it names an IP literal. Malformed entry text is refused by
the parser and is covered by the parser's own tests. -/

namespace Pack.PluginGrantDeclaration

inductive NetKind
  | noNet
  | anyHost
  | hosts
  | rawTcp

structure Entry where
  wildcard : Bool
  literal : Bool

structure Grant where
  net : NetKind
  entries : List Entry
  httpTimeoutMs : Option Nat

def entryServable (e : Entry) : Bool := !(e.wildcard && e.literal)

def netServable : NetKind → List Entry → Bool
  | .noNet, _ => true
  | .anyHost, _ => true
  | .hosts, es => es.all entryServable
  | .rawTcp, _ => false

def timeoutServable : Option Nat → Bool
  | some 0 => false
  | _ => true

def servable (g : Grant) : Bool :=
  netServable g.net g.entries && timeoutServable g.httpTimeoutMs

theorem raw_tcp_is_never_servable (es : List Entry) (t : Option Nat) :
    servable { net := .rawTcp, entries := es, httpTimeoutMs := t } = false := by
  simp [servable, netServable]

theorem zero_timeout_is_never_servable (n : NetKind) (es : List Entry) :
    servable { net := n, entries := es, httpTimeoutMs := some 0 } = false := by
  cases n <;> simp [servable, netServable, timeoutServable]

theorem wildcard_over_ip_is_never_servable (es : List Entry) (t : Option Nat)
    (h : ({ wildcard := true, literal := true } : Entry) ∈ es) :
    servable { net := .hosts, entries := es, httpTimeoutMs := t } = false := by
  have hall : es.all entryServable = false := by
    rw [List.all_eq_false]
    exact ⟨{ wildcard := true, literal := true }, h, by decide⟩
  simp [servable, netServable, hall]

theorem plain_hostname_is_servable :
    servable { net := .hosts, entries := [{ wildcard := false, literal := false }], httpTimeoutMs := none } = true := by
  decide

theorem literal_address_is_servable :
    servable { net := .hosts, entries := [{ wildcard := false, literal := true }], httpTimeoutMs := none } = true := by
  decide

end Pack.PluginGrantDeclaration

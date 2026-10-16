/-! Host-mediated plugin network admission (#2300): the host admits each
request through `allowed`, follows no redirect (a 3xx goes back to the plugin
and is admitted afresh). Hostname entries reach public addresses only, checked
on the addresses the connection actually uses, so a name that resolves (or
later re-resolves) to an internal address is refused; only an entry naming an
IP literal grants a non-public address. -/

namespace ToolPolicy.PluginNetwork

structure V4 where
  a : Nat
  b : Nat
  c : Nat
  d : Nat
  deriving DecidableEq, Repr

def v4Public (ip : V4) : Bool :=
  !(ip.a == 0 || ip.a == 10 || ip.a == 127
    || (ip.a == 100 && 64 ≤ ip.b && ip.b < 128)
    || (ip.a == 169 && ip.b == 254)
    || (ip.a == 172 && 16 ≤ ip.b && ip.b < 32)
    || (ip.a == 192 && ip.b == 0 && ip.c == 0)
    || (ip.a == 192 && ip.b == 168)
    || (ip.a == 198 && (ip.b == 18 || ip.b == 19))
    || 224 ≤ ip.a)

structure V6 where
  s0 : Nat
  s1 : Nat
  s2 : Nat
  s3 : Nat
  s4 : Nat
  s5 : Nat
  s6 : Nat
  s7 : Nat
  deriving DecidableEq, Repr

def V4.ofSegments (hi lo : Nat) : V4 := ⟨hi / 256, hi % 256, lo / 256, lo % 256⟩

def v6Public (ip : V6) : Bool :=
  if ip.s0 == 0 && ip.s1 == 0 && ip.s2 == 0 && ip.s3 == 0 && ip.s4 == 0 && ip.s5 == 0xffff then
    v4Public (V4.ofSegments ip.s6 ip.s7)
  else if ip.s0 == 0x2002 then
    v4Public (V4.ofSegments ip.s1 ip.s2)
  else
    0x2000 ≤ ip.s0 && ip.s0 < 0x4000
      && !(ip.s0 == 0x2001 && (ip.s1 == 0 || ip.s1 == 0xdb8 || (0x10 ≤ ip.s1 && ip.s1 < 0x30)))

inductive Addr where
  | v4 (ip : V4)
  | v6 (ip : V6)
  deriving DecidableEq, Repr

def ipPublic : Addr → Bool
  | .v4 ip => v4Public ip
  | .v6 ip => v6Public ip

inductive Host where
  | name (domain : String)
  | ip (addr : Addr)
  deriving DecidableEq, Repr

inductive Pattern where
  | exact (host : Host)
  /-- `*.domain`: strict subdomains only, never the apex. -/
  | suffix (domain : String)
  deriving DecidableEq, Repr

structure Entry where
  pattern : Pattern
  plaintext : Bool
  port : Option Nat
  deriving DecidableEq, Repr

inductive Grant where
  | sealed
  | anyHost
  | hosts (entries : List Entry)
  deriving DecidableEq, Repr

structure Target where
  https : Bool
  host : Host
  port : Nat
  deriving DecidableEq, Repr

def defaultPort (https : Bool) : Nat := if https then 443 else 80

def patternMatches : Pattern → Host → Bool
  | .exact h, t => h == t
  | .suffix d, .name n => n.endsWith ("." ++ d)
  | .suffix _, .ip _ => false

def entryAdmits (e : Entry) (t : Target) : Bool :=
  patternMatches e.pattern t.host && (t.https || e.plaintext)
    && t.port == e.port.getD (defaultPort t.https)

def explicitLiteral (e : Entry) : Bool :=
  match e.pattern with
  | .exact (.ip _) => true
  | _ => false

def addressesAllowed (explicit : Bool) (addrs : List Addr) : Bool :=
  !addrs.isEmpty && (explicit || addrs.all ipPublic)

def allowed : Grant → Target → List Addr → Bool
  | .sealed, _, _ => false
  | .anyHost, t, addrs => t.https && t.port == 443 && addressesAllowed false addrs
  | .hosts es, t, addrs =>
      es.any fun e => entryAdmits e t && addressesAllowed (explicitLiteral e) addrs

theorem sealed_reaches_nothing (t : Target) (addrs : List Addr) :
    allowed .sealed t addrs = false := rfl

theorem allowed_has_addresses (g : Grant) (t : Target) (addrs : List Addr)
    (h : allowed g t addrs = true) : addrs ≠ [] := by
  intro hnil
  subst hnil
  cases g <;> simp [allowed, addressesAllowed] at h

theorem plaintext_needs_entry (g : Grant) (t : Target) (addrs : List Addr)
    (h : allowed g t addrs = true) (hp : t.https = false) :
    ∃ es e, g = .hosts es ∧ e ∈ es ∧ entryAdmits e t = true ∧ e.plaintext = true := by
  cases g with
  | sealed => simp [allowed] at h
  | anyHost => simp [allowed, hp] at h
  | hosts es =>
      simp only [allowed, List.any_eq_true, Bool.and_eq_true] at h
      obtain ⟨e, he, hadmit, _⟩ := h
      refine ⟨es, e, rfl, he, hadmit, ?_⟩
      simp only [entryAdmits, hp, Bool.false_or, Bool.and_eq_true] at hadmit
      exact hadmit.1.2

theorem internal_needs_literal (g : Grant) (t : Target) (addrs : List Addr)
    (h : allowed g t addrs = true) (hi : ∃ x ∈ addrs, ipPublic x = false) :
    ∃ es e, g = .hosts es ∧ e ∈ es ∧ entryAdmits e t = true ∧ explicitLiteral e = true := by
  obtain ⟨x, hx, hpub⟩ := hi
  have notAll : addrs.all ipPublic = false := by
    rw [List.all_eq_false]
    exact ⟨x, hx, by simp [hpub]⟩
  cases g with
  | sealed => simp [allowed] at h
  | anyHost => simp [allowed, addressesAllowed, notAll] at h
  | hosts es =>
      simp only [allowed, List.any_eq_true, Bool.and_eq_true] at h
      obtain ⟨e, he, hadmit, haddr⟩ := h
      refine ⟨es, e, rfl, he, hadmit, ?_⟩
      cases hl : explicitLiteral e
      · simp [addressesAllowed, hl, notAll] at haddr
      · rfl

/-- Admission never depends on a hostname resolving the same way twice. -/
theorem hostname_entry_public_only (es : List Entry) (t : Target) (addrs : List Addr)
    (h : allowed (.hosts es) t addrs = true)
    (hnolit : ∀ e ∈ es, explicitLiteral e = false) : addrs.all ipPublic = true := by
  simp only [allowed, List.any_eq_true, Bool.and_eq_true] at h
  obtain ⟨e, he, _, haddr⟩ := h
  have := by simpa [addressesAllowed, hnolit e he] using haddr
  simpa using this.2

end ToolPolicy.PluginNetwork

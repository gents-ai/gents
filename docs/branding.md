# Gent — Name & Brand Brief

> Working brief for naming and brand identity. `defra-agent` is the engineering
> name; **Gent** is the product/launch name. This doc captures the decision and
> the reasoning so it survives past any single conversation.

---

## The name: Gent

A DefraDB-backed agent runtime where every agent is a DID-backed cryptographic
identity that signs every document it writes, behaves according to formally
proven (Lean 4) state machines, and replicates over P2P gossip. The name has to
carry *trust, delegated authority, and good conduct* — not "database" and not
"agent framework #4,001."

**Gent** does that:

- **gent** = the tail of a*gent* — honest about what it is, zero mysticism.
- **Gent** = a trusted actor of good conduct — which is *literally* what the
  proofs guarantee.
- Clean lowercase CLI: `gent init`, `gent server`, `gent chat`.
- Pluralizes for the fleet/P2P story — *a fleet of gents* already sounds like a
  product.
- It says nothing about the database. DefraDB is *how*, not *why*; you never want
  the substrate in the name.

### Why this name and not the obvious one (Sigil)

We pressure-tested **Sigil** first. It's dead on arrival in this market:

- **Sigil by NOMARK** (`sigilsec.ai`) — "Automated Security Auditing for AI Agent
  Code." Adjacent.
- **SIGIL Protocol** (`sigil-protocol.org`) — "Sovereign Identity-Gated
  Interaction Layer." Identity binding for agents, tamper-evident audit trails,
  **Rust implementation, formal specification, patent pending (filed
  2026-02-23).** That is our elevator pitch with a different logo, *plus* a
  patent application in flight.
- Also the long-standing Sigil EPUB editor (different category, but SEO noise).

**Gent**, by contrast, is genuinely clean. The only collision is `gent-lang/gent`
on GitHub (a solo alpha "language for AI agents" — different category, not
commercial). Everything else is the usual `Gent-` prefix soup (Gentrace, Genta,
Gentek, Gentic) — nobody owns the bare word as a product brand. The domains are
available, which for a real four-letter English-ish word is genuinely rare.

---

## The metaphor: butler, not gentleman

This is the heart of the brand. Early instinct was "gentleman," but that skews
toward a male / old-money / exclusive *club* — a blind spot we're deliberately
avoiding. **Butler** is the right metaphor and it maps tighter to the
architecture on every axis:

| Butler trait | What it is in the product |
|---|---|
| Acts on your behalf with delegated authority | An agent. Exactly. |
| Discretion; keeps confidences | Least-privilege principals, document-level permissions |
| Runs the whole estate | Orchestrating a fleet of agents across nodes |
| Anticipates, never oversteps | Proven-bounded behavior — does the job and stops |
| Impeccable conduct | Lean-proven state machines: provably well-behaved |

A butler is a **role, not a club** — inclusive by default. And it hands us a
ready-made cultural range to set tone by the room:

- **Jeeves** — the competent operator quietly smarter than the boss, who handles
  everything. (Playful, capable.)
- **Alfred** — the trusted infra/ops hand behind the hero. (Serious, dependable.)
- **Carson** — runs the entire household/estate. (Fleet orchestration at scale.)

### Free vocabulary

The metaphor hands us the words for everything:

- **the estate** = your infrastructure
- **the household** = the fleet of agents
- **at your service** = the document-driven request/response model
- **conduct / good behavior** = the formal proofs
- **discretion** = the permission model

One coherent register for docs, marketing, feature names, and the enterprise
deck — all from one well, without sounding forced. The name and the substance
point the same direction. That's the rare and valuable part.

---

## Logo & identity system

### Wordmark or symbol? Both — symbol-forward.

Four-letter names *want* a memorable glyph; the word alone is too small to carry
recognition. Lead with the symbol, pair with a wordmark.

### The symbol: a bow tie

Formal *with* personality — which is the whole enterprise/prosumer thesis in one
object. Boardroom-legible, but with taste and a human behind it. It lets us be
playful *because* the underlying form is formal.

The geometry is a bonus — a bow tie is also:

- a **knot** → binding (we do cryptographic *identity binding*) — lead with this
  for the trust pitch: *tying identity to action*
- two triangles meeting point-to-point → a **P2P link** (gossip replication)
- an **hourglass** → a **lifecycle / state machine** (the Lean-proven core)

Pick whichever story fits the room.

### The actual brand is the print system

Not a logo — a **system**, and the system mirrors the architecture. The bow tie
is a canvas; different prints map to different parts of the stack.

- **Enterprise = black tie.** Monochrome, textured, restrained. The serious
  default. Top tier = black tie **with a gold accent** — so gold *means* "premium
  tier" instead of just decorating everything.
- **OSS = expressive prints.** Where people fall in love with the brand.

**The OSS print = an original "developer toile."** Like toile de Jouy (those
repeating scenic prints), but built from *our own* dev iconography: terminal
prompts, git graphs, braces and semicolons, little state-machine diagrams, DIDs,
command snippets. Same playful density as a Vineyard Vines / Southern Proper
print, but completely ownable and legally clean — "Vineyard Vines for people who
live in a terminal."

> **Do not** put third-party logos (gaming, other OSS projects) in the pattern.
> Trademark headaches, and it hitches our identity to theirs. Evoke that energy
> with original motifs instead.

### Two extensions worth banking

1. **Community prints as an engagement mechanic.** Conference-exclusive prints,
   seasonal prints, a community print contest, contributor-designed weaves.
   People wear swag they helped make — the brand recruits for you.
2. **Generative prints from identity.** Each principal's bow-tie weave
   deterministically generated from its DID. A pattern no one can fake because
   it's downstream of the cryptographic identity itself. The premium tech flex.

### Texture vs. clean: it's a fidelity ladder, not a choice

Most dev-tool logos are flat geometric SVGs. A textured, high-res, almost tactile
mark reads *crafted* — it stands out. But texture dies at a favicon and in
monochrome. So build both:

- **Hero mark** — rich, textured, high-res bow tie. Web, decks, swag, README
  hero. This is where the "it's a brand, it has a feel" energy lives.
- **Working glyph** — simplified single-color bow-tie/knot silhouette that
  survives a 16px favicon, a monochrome terminal, an embroidered cap, and a
  contributor redrawing it from memory.

Go maximal where you have pixels; minimal where you don't.

### Color

- Anchor in one confident base — Southern-prep palette has real options:
  **oxblood, deep navy, hunter green.**
- Let the **prints** carry the color explosion, not the base mark.
- **Gold = accent, specifically the enterprise tier** — not the whole identity.
  (Full-gold reads crypto-bro / cheap-luxe fast.)

---

## Positioning notes

- **Audience:** enterprise is where the revenue is; prosumer/OSS free tier is the
  top of funnel and the community. The name and brand must serve both — `gent`
  the lowercase CLI is hacker-native; "Gent" the butler is boardroom-safe. Protect
  that dual read; don't let the brand drift fully cute *or* fully corporate.
- **Trademark strength:** "gent" reads as a clip of "agent," so it lands around
  *suggestive/descriptive* — easy to teach and market, harder to defend as a bare
  word. That's a deliberate trade (instant comprehension over easy defense).
  Mitigate by registering a **stylized mark** (the bow-tie wordmark treatment),
  which is standard practice anyway.

## Open checks before locking in

- [ ] Domains (`.dev` / `.ai` / `.com`) — secure the set
- [ ] GitHub org (`gent` / `genthq` / `gent-dev`) — note the `gent` user may be
      the alpha-lang author
- [ ] Package namespaces: crates.io, npm (if a JS SDK ships)
- [ ] Social handles
- [ ] Trademark knockout search, Class 9 (software) + Class 42 (SaaS)
- [ ] Decide company-name vs product-name (is the company Gent too?)

---

*One-line summary:* **Gent** — a butler for your infrastructure. A name that hands
us the *conduct / stewardship / discretion* story, which is exactly the story our
formal-proofs-and-cryptographic-identity architecture is already telling.

Re-attack patch `{{ doc.patch_id }}` for cluster `{{ doc.cluster_id }}` after
maintainer verdict `{{ doc.verdict }}`. Review reason: {{ doc.reason }}

Call each bounded read exactly once: `read_defense_root_cause_cluster`,
`read_defense_contract_review`, `read_defense_patch_candidate`, and
`read_defense_patch_validation`. If the patch or prior review is skipped, write
one security review with `verdict=SKIP`. Otherwise independently trace the
original entry-to-sink path, inspect sibling variants, try at least one concrete
bypass hypothesis, and compare the change to the contract review. Treat prior
acceptance as evidence, not authority.

Call `write_defense_patch_security_review` exactly once with
`security_review_id={{ doc.patch_id }}:security`; verdict exactly `ACCEPT`,
`REJECT`, or `SKIP`; `original_path_closed=yes|no|unknown`; concrete sibling
variants, bypass result, contract alignment, and source/diff evidence. ACCEPT
requires prior maintainer ACCEPT, mechanically applicable validation with no
required gate failed, no demonstrated bypass, and contract alignment. Do not
supply runtime-filled ids or expected total.

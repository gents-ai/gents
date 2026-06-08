# Competitive positioning

This folder keeps the agent-framework and protocol positioning work in one
place so it is easy to find without spreading strategy notes through the main
spec and audit directories.

Use this folder for customer-facing ecosystem maps, competitive analysis,
adapter roadmaps, and verification notes for those claims.

## Documents

- `protocol-product-positioning-map.md`: how Defra Agent maps to MCP, A2A,
  external Agent Communication Protocol, ANP, LangGraph, CrewAI, OpenAI Agents
  SDK, AutoGen, and Microsoft Agent Framework.
- `adapter-projection-template-roadmap.md`: concrete adapter, projection, and
  pattern-template work that follows from the positioning map.
- `protocol-product-positioning-verification.md`: verification notes for the
  positioning claims against local Defra Agent code and upstream source
  material.

## Product thesis

Defra Agent should not compete by cloning every framework API. It should expose
familiar protocol and framework surfaces over the same Defra-native substrate:
signed agent documents, DID-keyed identities, durable work documents,
DefraDB-backed ACL, P2P document replication, runtime lineage, and projection
exports.

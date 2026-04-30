import type { PeerAddRequest } from "../../lib/types";

type JsonRecord = Record<string, unknown>;

const LEGACY_NAME_DERIVED_DID_PREFIX = "did:defra-agent:";

export function parsePeerConnectionJson(input: string): PeerAddRequest {
  const value = parseJsonFromText(input);
  const record = asRecord(value);

  if (!record) {
    throw new Error("Connection JSON must be an object");
  }

  const agentDid =
    stringAt(record, "agentDid") ??
    stringAt(record, "agent_did") ??
    stringAt(record, "runtime_state.agentDid") ??
    stringAt(record, "runtime_state.agent_did");
  const addr =
    stringAt(record, "addr") ??
    stringAt(record, "address") ??
    stringAt(record, "p2pAddress") ??
    stringAt(record, "p2p_address") ??
    stringAt(record, "p2pShareableAddress") ??
    stringAt(record, "p2p_shareable_address") ??
    stringAt(record, "p2p.p2pShareableAddress") ??
    stringAt(record, "p2p.p2p_shareable_address") ??
    firstStringAt(record, "p2pListenAddresses") ??
    firstStringAt(record, "p2p_listen_addresses") ??
    firstStringAt(record, "runtime_state.p2p_listen_addresses") ??
    firstStringAt(record, "p2p.p2p_listen_addresses");

  if (!agentDid || !addr) {
    throw new Error(
      "Connection JSON must include agent_did and a P2P address",
    );
  }
  const validatedAgentDid = validateAgentDid(agentDid);
  const graphql =
    stringAt(record, "desktopGraphql") ??
    stringAt(record, "desktop_graphql") ??
    stringAt(record, "graphql") ??
    stringAt(record, "checks.graphql.endpoint") ??
    stringAt(record, "runtime_state.graphql");

  return {
    label:
      stringAt(record, "label") ??
      stringAt(record, "agentLabel") ??
      stringAt(record, "agent_label") ??
      stringAt(record, "agentName") ??
      stringAt(record, "agent_name") ??
      stringAt(record, "runtime_state.agentName") ??
      stringAt(record, "runtime_state.agent_name") ??
      inferLabel(validatedAgentDid),
    agentDid: validatedAgentDid,
    addr,
    ...(graphql ? { graphql } : {}),
  };
}

export function validateAgentDid(agentDid: string) {
  const trimmed = agentDid.trim();
  if (!trimmed) {
    throw new Error("Agent DID is required");
  }
  if (trimmed.startsWith(LEGACY_NAME_DERIVED_DID_PREFIX)) {
    throw new Error(
      "Agent DID must be the key-derived DID from defra-agent init/status, not did:defra-agent:<name>",
    );
  }
  return trimmed;
}

function parseJsonFromText(input: string): unknown {
  const trimmed = input.trim();
  if (!trimmed) {
    throw new Error("Connection JSON is empty");
  }

  try {
    return JSON.parse(trimmed);
  } catch {
    const start = trimmed.indexOf("{");
    const end = trimmed.lastIndexOf("}");
    if (start === -1 || end <= start) {
      throw new Error("Connection JSON is not valid JSON");
    }
    return JSON.parse(trimmed.slice(start, end + 1));
  }
}

function stringAt(record: JsonRecord, path: string) {
  const value = valueAt(record, path);
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function firstStringAt(record: JsonRecord, path: string) {
  const value = valueAt(record, path);
  if (!Array.isArray(value)) {
    return null;
  }
  return value.find(
    (candidate): candidate is string =>
      typeof candidate === "string" && candidate.trim().length > 0,
  ) ?? null;
}

function valueAt(record: JsonRecord, path: string): unknown {
  let cursor: unknown = record;
  for (const segment of path.split(".")) {
    const current = asRecord(cursor);
    if (!current) {
      return undefined;
    }
    cursor = current[segment];
  }
  return cursor;
}

function asRecord(value: unknown): JsonRecord | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : null;
}

function inferLabel(agentDid: string) {
  const parts = agentDid.split(":").filter(Boolean);
  const tail = parts[parts.length - 1];
  return tail || "Agent";
}

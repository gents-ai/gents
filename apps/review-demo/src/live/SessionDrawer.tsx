import { useEffect, useState } from "react";

import { documentForNode } from "../graph/documentForNode.ts";
import type { GraphNode, ReviewSnapshot } from "../graph/types.ts";
import { loadSession, type SessionPayload } from "./pollRuntime.ts";

type SessionDrawerProps = {
  node: GraphNode | null;
  snapshot: ReviewSnapshot;
};

export function SessionDrawer({ node, snapshot }: SessionDrawerProps) {
  const [payload, setPayload] = useState<SessionPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [openTool, setOpenTool] = useState<number | null>(null);

  useEffect(() => {
    setOpenTool(null);
    if (!node?.requestId) {
      setPayload(null);
      setError(null);
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const next = await loadSession(node.requestId!, node.sessionId);
        if (!cancelled) {
          setPayload(next);
          setError(null);
        }
      } catch (cause) {
        if (!cancelled) {
          setError(cause instanceof Error ? cause.message : String(cause));
        }
      }
    };
    void tick();
    const timer = window.setInterval(() => {
      void tick();
    }, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [node?.requestId, node?.sessionId]);

  useEffect(() => {
    if (openTool === null) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpenTool(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openTool]);

  if (!node) {
    return (
      <section className="session-drawer">
        <p className="eyebrow">Session</p>
        <h2>Select a document</h2>
        <p className="muted">
          Click ReviewJob, an area, or a scan to see the written document, the interpolated
          task prompt, and tool calls.
        </p>
      </section>
    );
  }

  const doc = documentForNode(node, snapshot);
  const promptTokens = payload?.promptTokens ?? 0;
  const completionTokens = payload?.completionTokens ?? 0;
  const totalTokens = promptTokens + completionTokens;
  const tokenLabel =
    totalTokens > 0
      ? `${totalTokens.toLocaleString()} (${promptTokens.toLocaleString()} in · ${completionTokens.toLocaleString()} out)`
      : "—";
  const tools = payload?.tools ?? [];
  const selectedTool = openTool !== null ? tools[openTool] : null;

  return (
    <section className="session-drawer">
      <p className="eyebrow">Session</p>
      <h2>{node.label}</h2>
      {node.detail ? <p className="session-detail">{node.detail}</p> : null}
      <dl className="session-meta">
        <div>
          <dt>state</dt>
          <dd>{node.state}</dd>
        </div>
        <div>
          <dt>request</dt>
          <dd className="mono">{node.requestId ?? "—"}</dd>
        </div>
        <div>
          <dt>tokens</dt>
          <dd>{tokenLabel}</dd>
        </div>
      </dl>
      {error ? <p className="error-line">{error}</p> : null}
      <h3>{doc ? doc.collection : "Document"}</h3>
      {doc ? (
        <pre className="doc-json">{JSON.stringify(doc.fields, null, 2)}</pre>
      ) : (
        <p className="muted">This node has no document yet.</p>
      )}
      <h3>Task prompt</h3>
      <pre className="prompt">{payload?.prompt || "Waiting for interpolated prompt…"}</pre>
      <h3>Tools</h3>
      {tools.length === 0 ? (
        <p className="muted">No tool calls yet.</p>
      ) : (
        <ul className="tool-list">
          {tools.map((tool, index) => (
            <li key={`${tool.tool_name ?? "tool"}-${index}`}>
              <button type="button" className="tool-open" onClick={() => setOpenTool(index)}>
                <code>{tool.tool_name}</code>
                <span>{tool.lifecycle_state || tool.status || ""}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {selectedTool ? (
        <div className="modal-scrim" onClick={() => setOpenTool(null)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-label={selectedTool.tool_name ?? "tool call"}
            onClick={(event) => event.stopPropagation()}
          >
            <header className="modal-head">
              <div>
                <p className="eyebrow">Tool call</p>
                <h2>
                  <code>{selectedTool.tool_name}</code>
                </h2>
              </div>
              <button type="button" className="ghost-button" onClick={() => setOpenTool(null)}>
                Close
              </button>
            </header>
            <p className="muted">
              {selectedTool.lifecycle_state || selectedTool.status || "unknown status"}
            </p>
            <h3>Args</h3>
            <pre className="doc-json">{pretty(selectedTool.args)}</pre>
            <h3>Result</h3>
            <pre className="doc-json">{pretty(selectedTool.result)}</pre>
          </div>
        </div>
      ) : null}
    </section>
  );
}

function pretty(raw?: string): string {
  if (!raw) {
    return "—";
  }
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

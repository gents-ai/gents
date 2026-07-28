import { useCallback, useRef, useState } from "react";

import { explainToolSurface } from "@source-inc/gents-desktop-client";
import type { ToolSurfaceExplanationView } from "@source-inc/gents-desktop-client";

/// The behavior's RESOLVED tool surface, computed by the runtime's explain
/// machinery over the live documents — what the model actually gets, not
/// what the raw ToolSelection says. Fetched on expand/refresh only.
export function BehaviorToolSurface({
  agentDid,
  behaviorId,
}: {
  agentDid: string;
  behaviorId: string | null;
}) {
  const [open, setOpen] = useState(false);
  const [explanation, setExplanation] = useState<ToolSurfaceExplanationView | null>(
    null,
  );
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  const load = useCallback(async () => {
    if (!behaviorId) {
      return;
    }
    const generation = ++generationRef.current;
    setLoading(true);
    setError(null);
    try {
      const next = await explainToolSurface(agentDid, behaviorId);
      if (generationRef.current === generation) {
        setExplanation(next);
      }
    } catch (err) {
      if (generationRef.current === generation) {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (generationRef.current === generation) {
        setLoading(false);
      }
    }
  }, [agentDid, behaviorId]);

  if (!behaviorId) {
    return null;
  }

  return (
    <section className="behavior-tool-surface" data-testid="behavior-tool-surface">
      <button
        aria-expanded={open}
        className="ghost-button behavior-tool-surface-toggle"
        data-testid="behavior-tools-explain-toggle"
        onClick={() => {
          setOpen((value) => {
            if (!value && !explanation && !loading) {
              void load();
            }
            return !value;
          });
        }}
        type="button"
      >
        {open ? "Hide resolved tools" : "Resolved tools"}
      </button>

      {open ? (
        <div className="behavior-tool-surface-body">
          <div className="behavior-tool-surface-toolbar">
            <p className="eyebrow">What this behavior actually gets</p>
            <button
              className="ghost-button"
              data-testid="behavior-tools-explain-refresh"
              disabled={loading}
              onClick={() => void load()}
              type="button"
            >
              {loading ? "Loading..." : "Refresh"}
            </button>
          </div>

          {error ? (
            <p
              className="behavior-tool-surface-error"
              data-testid="behavior-tools-explain-error"
              role="alert"
            >
              Explanation failed: {error}
            </p>
          ) : null}

          {explanation ? (
            <>
              <p className="muted behavior-tool-surface-meta">
                {[
                  `policy: ${explanation.toolPolicySemantics}`,
                  `ceiling: ${explanation.ceilingSource}`,
                  explanation.mcpServicesOnline
                    ? "MCP services online"
                    : "MCP services offline",
                ].join(" · ")}
              </p>
              <ToolNameList
                label="Available tools"
                names={stringArray(explanation.surface.tool_names)}
              />
              <ReasonGroups
                label="Excluded"
                groups={stringMap(explanation.surface.excluded)}
              />
              <ReasonGroups
                label="Unavailable right now"
                groups={stringMap(explanation.surface.unavailable)}
              />
              {stringArray(
                (explanation.surface.warnings as unknown[])?.map((warning) =>
                  typeof warning === "object" && warning !== null
                    ? String((warning as Record<string, unknown>).message ?? "")
                    : String(warning),
                ),
              ).map((message) => (
                <p className="behavior-tool-surface-warning" key={message}>
                  {message}
                </p>
              ))}
            </>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string" && !!item)
    : [];
}

function stringMap(value: unknown): Array<[string, string[]]> {
  if (typeof value !== "object" || value === null) {
    return [];
  }
  return Object.entries(value as Record<string, unknown>).map(([key, reasons]) => [
    key,
    stringArray(reasons),
  ]);
}

function ToolNameList({ label, names }: { label: string; names: string[] }) {
  if (!names.length) {
    return (
      <p className="muted">
        {label}: none — the intersection with the ceiling is empty.
      </p>
    );
  }
  return (
    <div className="behavior-tool-surface-group">
      <p className="eyebrow">{label}</p>
      <div className="behavior-tool-surface-names" data-testid="resolved-tool-names">
        {names.map((name) => (
          <span className="chip" key={name}>
            {name}
          </span>
        ))}
      </div>
    </div>
  );
}

function ReasonGroups({
  label,
  groups,
}: {
  label: string;
  groups: Array<[string, string[]]>;
}) {
  if (!groups.length) {
    return null;
  }
  return (
    <div className="behavior-tool-surface-group">
      <p className="eyebrow">{label}</p>
      <ul className="behavior-tool-surface-reasons">
        {groups.map(([name, reasons]) => (
          <li key={name}>
            <span className="mono">{name}</span>
            {reasons.length ? <span> — {reasons.join("; ")}</span> : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

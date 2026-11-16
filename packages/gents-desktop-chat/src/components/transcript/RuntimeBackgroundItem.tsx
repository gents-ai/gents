import { useState } from "react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

type AutomatedInput = Extract<RenderedTimelineItem, { kind: "automatedInput" }>;

function details(content: string | null) {
  if (!content || typeof DOMParser === "undefined") return null;
  const document = new DOMParser().parseFromString(content, "application/xml");
  const root = document.documentElement;
  if (document.querySelector("parsererror")) return null;
  if (root.tagName === "agent-notification")
    return root.textContent || "No additional details.";
  if (root.tagName !== "tool-completion") return null;
  return root.querySelector("result")?.textContent || "No additional details.";
}

function statusLabel(
  origin: Extract<AutomatedInput["origin"], { kind: "backgroundCompletion" }>,
) {
  switch (origin.statusKind) {
    case "error":
      return "Background action failed";
    case "success":
      return "Background action completed";
    default:
      return "Background activity";
  }
}

export function RuntimeBackgroundItem({
  item,
  origin,
}: {
  item: AutomatedInput;
  origin: Extract<AutomatedInput["origin"], { kind: "backgroundCompletion" }>;
}) {
  const [open, setOpen] = useState(false);
  const ready = item.reconstruction.state === "ready";
  const result = ready ? details(item.content) : null;
  return (
    <details
      className="runtime-background-event"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span>{statusLabel(origin)}</span>
        {origin.toolName ? (
          <span className="runtime-background-tool">{origin.toolName}</span>
        ) : null}
      </summary>
      {open &&
        (ready ? (
          <pre>{result ?? "Background details are unavailable."}</pre>
        ) : item.reconstruction.state === "loading" ? (
          <p role="status">Loading background details…</p>
        ) : (
          <p>Background details are unavailable.</p>
        ))}
    </details>
  );
}

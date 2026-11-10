import { useState } from "react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

type AutomatedInput = Extract<RenderedTimelineItem, { kind: "automatedInput" }>;

function completion(content: string | null) {
  if (!content || typeof DOMParser === "undefined") return null;
  const document = new DOMParser().parseFromString(content, "application/xml");
  const root = document.documentElement;
  if (document.querySelector("parsererror")) return null;
  if (root.tagName === "agent-notification")
    return {
      label: "Background activity",
      result: root.textContent || "No additional details.",
    };
  if (root.tagName !== "tool-completion") return null;
  const status = root.getAttribute("status");
  return {
    label:
      status === "failed"
        ? "Background action failed"
        : status === "cancelled"
          ? "Background action cancelled"
          : status === "completed" || status === "complete"
            ? "Background action completed"
            : "Background activity",
    result:
      root.querySelector("result")?.textContent || "No additional details.",
  };
}

export function RuntimeBackgroundItem({ item }: { item: AutomatedInput }) {
  const [open, setOpen] = useState(false);
  const result =
    item.reconstruction.state === "ready" ? completion(item.content) : null;
  return (
    <details
      className="runtime-background-event"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>{result?.label ?? "Background activity"}</summary>
      {open &&
        (item.reconstruction.state === "ready" ? (
          <pre>{result?.result ?? "Background details are unavailable."}</pre>
        ) : item.reconstruction.state === "loading" ? (
          <p role="status">Loading background details…</p>
        ) : (
          <p>Background details are unavailable.</p>
        ))}
    </details>
  );
}

import { useState } from "react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

type AutomatedInput = Extract<RenderedTimelineItem, { kind: "automatedInput" }>;

export function RuntimeBackgroundItem({ item }: { item: AutomatedInput }) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="runtime-background-event"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>Background activity</summary>
      {open &&
        (item.reconstruction.state === "ready" ? (
          <pre>{item.content || "No additional details."}</pre>
        ) : (
          <p>Background details are unavailable.</p>
        ))}
    </details>
  );
}

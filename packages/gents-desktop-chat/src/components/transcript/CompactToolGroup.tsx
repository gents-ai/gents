import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { ToolGroup } from "./ToolGroup.js";
import {
  actionEntries,
  actionKind,
  runningAction,
  type ActionEntry,
} from "./actionPresentation.js";

export function coalesceToolGroups(items: RenderedTimelineItem[]) {
  const result: RenderedTimelineItem[] = [];
  for (const item of items) {
    const previous = result[result.length - 1];
    if (previous?.kind === "toolGroup" && item.kind === "toolGroup") {
      result[result.length - 1] = {
        ...previous,
        tools: [...previous.tools, ...item.tools],
      };
    } else {
      result.push(item);
    }
  }
  return result;
}

function completedLabel(entries: ActionEntry[]) {
  const counts = new Map<string, number>();
  for (const entry of entries) {
    const { tool } = entry;
    const presentation =
      actionKind(entry) === "command"
        ? "command"
        : tool.reconstruction.state === "ready"
          ? tool.presentation.kind
          : "generic";
    const kind = ["fileRead", "fileEdit", "command"].includes(presentation)
      ? presentation
      : "generic";
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return [...counts]
    .map(([kind, count]) => {
      switch (kind) {
        case "fileRead":
          return "explored files";
        case "fileEdit":
          return "edited files";
        case "command":
          return `ran ${count} ${count === 1 ? "command" : "commands"}`;
        default:
          return `ran ${count} ${count === 1 ? "action" : "actions"}`;
      }
    })
    .join(", ")
    .replace(/^./, (letter) => letter.toUpperCase());
}

function ActivityDetails({
  tools,
  expandedItemKey,
  onClose,
}: {
  tools: RenderedToolCallView[];
  expandedItemKey?: string;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const title = useId();
  useLayoutEffect(() => {
    const element = dialog.current!;
    element.showModal();
    element
      .querySelector(".tool-item[open]")
      ?.scrollIntoView({ block: "nearest" });
    return () => element.close();
  }, []);
  return createPortal(
    <dialog
      ref={dialog}
      className="tool-activity-dialog"
      aria-labelledby={title}
      onClose={onClose}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const bounds = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX < bounds.left ||
          event.clientX > bounds.right ||
          event.clientY < bounds.top ||
          event.clientY > bounds.bottom
        ) {
          event.currentTarget.close();
        }
      }}
    >
      <header className="tool-activity-header">
        <h2 id={title}>
          Actions <span>{actionEntries(tools).length}</span>
        </h2>
        <button
          type="button"
          aria-label="Close actions"
          onClick={() => dialog.current?.close()}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m6 6 12 12M6 18 18 6" />
          </svg>
        </button>
      </header>
      <div className="tool-activity-body">
        <ToolGroup
          tools={tools}
          compactDetails
          expandedItemKey={expandedItemKey}
        />
      </div>
    </dialog>,
    document.body,
  );
}

export function CompactToolGroup({ tools }: { tools: RenderedToolCallView[] }) {
  const [selected, setSelected] = useState<ActionEntry | null>(null);
  if (tools.length === 0) return null;
  const sections: ActionEntry[][] = [];
  for (const entry of actionEntries(tools)) {
    const previous = sections.at(-1);
    const status = entry.tool.statusKind.toLowerCase();
    if (
      status !== "running" &&
      previous?.[0]?.tool.statusKind.toLowerCase() === status
    )
      previous.push(entry);
    else sections.push([entry]);
  }
  return (
    <>
      {sections.map((entries) => {
        const entry = entries[0]!;
        const status = entry.tool.statusKind.toLowerCase();
        const running = status === "running";
        const failed = status === "error";
        const label = running
          ? runningAction(entry)
          : failed
            ? `${entries.length} ${entries.length === 1 ? "action failed" : "actions failed"}`
            : status === "success"
              ? completedLabel(entries)
              : "Action status unavailable";
        return (
          <button
            key={entry.tool.itemKey}
            type="button"
            className={`tool-activity-row${running ? " is-running" : ""}${failed ? " has-errors" : ""}`}
            aria-haspopup="dialog"
            aria-expanded={
              selected !== null &&
              entries.some(
                (item) => item.tool.itemKey === selected.tool.itemKey,
              )
            }
            aria-label={`View actions: ${label}`}
            onClick={() => setSelected(entry)}
          >
            <svg
              className="tool-activity-icon"
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              {running ? (
                <circle cx="12" cy="12" r="8" />
              ) : (
                <path d="M4 4h6a3 3 0 0 1 3 3v14a4 4 0 0 0-4-2H4V4Zm16 0h-4a3 3 0 0 0-3 3m0 14a4 4 0 0 1 4-2h3V4Z" />
              )}
            </svg>
            <span className="tool-activity-label" title={label}>
              {label}
            </span>
            <svg
              className="tool-activity-chevron"
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path d="m9 5 7 7-7 7" />
            </svg>
          </button>
        );
      })}
      {selected ? (
        <ActivityDetails
          tools={tools}
          expandedItemKey={
            selected.tool.statusKind.toLowerCase() === "success"
              ? undefined
              : selected.tool.itemKey
          }
          onClose={() => setSelected(null)}
        />
      ) : null}
    </>
  );
}
